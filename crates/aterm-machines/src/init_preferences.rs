// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local `aterm init` desktop preferences.

use crate::facts::{KeyboardCohorts, KeyboardCoverage};
use crate::init_observe::{LastReading, Reading};
use crate::items::catalog::{CATALOG, CatalogRow, Lit};
use crate::items::defaults::{self, Target};
use crate::items::{Activation, Ctx, Item, keyboard, widgets};
use crate::model::{Effect, PValue, Probe, Read, User};
use crate::plist::Entries;
#[cfg(test)]
use crate::port::KeyboardPrefix;
use crate::port::{
    Cmd, KeyboardMapChange, KeyboardServiceObservation, KeyboardServiceState, System,
};
use aterm_types::macos_setup::{LiveKeyboardMapping, Selection, Setting};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

const SRC: &str = "HIDKeyboardModifierMappingSrc";
const WINDOW_MANAGER: &str = "com.apple.WindowManager";
const FINDER: &str = "com.apple.finder";
const DOCK: &str = "com.apple.dock";
const DST: &str = "HIDKeyboardModifierMappingDst";
const HIDUTIL: &str = aterm_uds::os_path!("hidutil");
#[cfg(test)]
const NATIVE_MAPPING: &str = "HIDKeyboardModifierMappingPairs";
const DOCK_APPS: [(&str, &str, &str); 3] = [
    ("/Applications/aterm.app", "com.aterm.aterm", "aterm"),
    (
        "/System/Applications/System Settings.app",
        "com.apple.systempreferences",
        "System Settings",
    ),
    ("/Applications/Safari.app", "com.apple.Safari", "Safari"),
];

type PreferenceItem = (Setting, Box<dyn Item>);

/// Build only the selected preferences, before reading any preference domain.
/// # Errors
/// An absent or ambiguous user, or an invalid selected keyboard model.
#[cfg(test)]
pub(crate) fn selected_items(ctx: &Ctx, selected: Selection) -> Result<Vec<Box<dyn Item>>, String> {
    Ok(
        selected_tagged_items(ctx, selected, &KeyboardCohorts::EMPTY)?
            .into_iter()
            .map(|(_, item)| item)
            .collect(),
    )
}

/// Selected items, each with the setting whose System State row it reports to. Build a
/// pass's preference items with one call: its widget, desktop and Dock items share one
/// [`DesktopActivation`], and the engine settles an id once, with its last item's activation, so
/// a second call's activation under the same id would never relaunch its families.
///
/// # Errors
/// An absent or ambiguous user, or an invalid selected keyboard model.
pub(crate) fn selected_tagged_items(
    ctx: &Ctx,
    selected: Selection,
    cohorts: &KeyboardCohorts,
) -> Result<Vec<PreferenceItem>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init preferences require exactly one local user".into());
    };
    let mut items: Vec<PreferenceItem> = Vec::new();
    if selected.contains(Setting::CapsLock) {
        let persistent = keyboard::expand(
            &keyboard::Table {
                caps_lock: Some("control".into()),
                models: if ctx.facts.arch == "arm64" {
                    vec!["0-0".into()]
                } else {
                    Vec::new()
                },
            },
            ctx,
        )?;
        items.extend(
            persistent
                .into_iter()
                .chain(attached_keyboard_items(ctx, cohorts)?)
                .map(|item| (Setting::CapsLock, item)),
        );
    }
    let activation = Arc::new(DesktopActivation::new(
        user,
        Reach::all(ctx.facts.full_disk_access),
    ));
    for (setting, kind) in desktop() {
        if selected.contains(setting) {
            items.push((setting, Box::new(Desktop::new(user, kind, &activation))));
        }
    }
    Ok(items)
}

/// The setup row of the desktop item with `id` (`consumer.widgets.notification-center@alice`: the
/// widgets row), for a caller following up the items a pass withheld ([`Item::withheld`]).
#[must_use]
pub fn setting_of(id: &str) -> Option<Setting> {
    let (name, _) = id.split_once('@')?;
    desktop()
        .find(|(_, kind)| kind.name() == name)
        .map(|(setting, _)| setting)
}

/// The System State row whose items write the preference `key` of `domain` (a ByHost key
/// too), read off the keys the items themselves write: a model's Caps Lock mapping, the
/// trackpad's keys, and each desktop item's. `None` for a key no item of this build writes,
/// such as one an older aterm backed up.
#[must_use]
pub fn setting_of_preference(domain: &str, key: &str) -> Option<Setting> {
    if domain == "-g" && keyboard::Model::of_key(key).is_some() {
        return Some(Setting::CapsLock);
    }
    crate::init_trackpad::setting_of(domain, key).or_else(|| {
        desktop()
            .find(|(_, kind)| {
                kind.keys()
                    .iter()
                    .any(|(each_domain, each_key, _)| *each_domain == domain && *each_key == key)
            })
            .map(|(setting, _)| setting)
    })
}

/// The ids of this build's items that `id`, from a record an older aterm wrote, stands for:
/// `init.widgets@<user>` stands for each `[consumer] widgets` row's own item (where `withheld`,
/// only those whose domain is in another app's container), and any other id for itself.
#[must_use]
pub fn current_ids(id: &str, withheld: bool) -> Vec<String> {
    match id.split_once('@') {
        Some((LEGACY_WIDGETS_ITEM, user)) => desktop()
            .filter_map(|(_, kind)| match kind {
                DesktopKind::Widget(row) if !withheld || defaults::contained(row.domain) => {
                    Some(format!("{}@{user}", row.id))
                }
                _ => None,
            })
            .collect(),
        _ => vec![id.to_owned()],
    }
}

const LEGACY_WIDGETS_ITEM: &str = "init.widgets";

fn desktop() -> impl Iterator<Item = (Setting, DesktopKind)> {
    CATALOG
        .iter()
        .filter(|row| row.preset == "widgets")
        .map(|row| (Setting::Widgets, DesktopKind::Widget(row)))
        .chain([
            (Setting::Desktop, DesktopKind::Desktop),
            (Setting::Dock, DesktopKind::Dock),
        ])
}

/// The settings of `selected` with a key in another app's container ([`defaults::contained`]):
/// the System State rows whose changes a pass without Full Disk Access withholds, and which its
/// follow-up makes ([`withheld_items`]).
#[must_use]
pub fn withheld_settings(selected: Selection) -> Selection {
    let mut settings = Selection::none();
    for (setting, kind) in desktop() {
        if selected.contains(setting)
            && kind
                .keys()
                .iter()
                .any(|(domain, _, _)| defaults::contained(domain))
        {
            settings.set(setting, true);
        }
    }
    settings
}

/// The follow-up of a pass that left keys in another app's container alone ([`Item::withheld`]),
/// each item with its setting ([`selected_tagged_items`]).
/// # Errors
/// An absent or ambiguous user.
pub fn withheld_items(ctx: &Ctx, selected: Selection) -> Result<Vec<PreferenceItem>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init preferences require exactly one local user".into());
    };
    let activation = Arc::new(DesktopActivation::new(
        user,
        Reach {
            full_disk_access: ctx.facts.full_disk_access,
            contained_only: true,
        },
    ));
    Ok(desktop()
        .filter(|(setting, kind)| {
            selected.contains(*setting)
                && kind
                    .keys()
                    .iter()
                    .any(|(domain, _, _)| defaults::contained(domain))
        })
        .map(|(setting, kind)| {
            let item: Box<dyn Item> = Box::new(Desktop::new(user, kind, &activation));
            (setting, item)
        })
        .collect())
}

/// Apply Caps Lock as Control to attached HID services without replacing the persistent modifier
/// preferences.
/// # Errors
/// The setup did not resolve exactly one local user.
pub(crate) fn attached_keyboard_items(
    ctx: &Ctx,
    cohorts: &KeyboardCohorts,
) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("input activation requires exactly one local user".into());
    };
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    let mut models: Vec<keyboard::Model> = ctx
        .facts
        .keyboards
        .iter()
        .map(|k| keyboard::Model {
            vendor: k.vendor,
            product: k.product,
        })
        .collect();
    models.sort();
    models.dedup();
    for model in models {
        items.push(Box::new(AttachedKeyboard {
            model,
            user: user.clone(),
            reading: LastReading::default(),
            expected: cohorts.expected(model.vendor, model.product)?,
        }));
    }
    Ok(items)
}

/// Development acceptance of the production native setter on an already-Control keyboard.
/// Adds only native Caps→Left Control, requiring an empty native table and exactly the same
/// user Caps→Left Control rule. Both before/after cohorts and all unrelated tables are checked.
///
/// # Errors
/// Console identity, the exact service or before-image differs, the backend refuses, or its
/// fresh service records do not verify the one intended addition.
#[cfg(any(test, debug_assertions))]
pub fn accept_native_caps_control(vendor: u32, product: u32, registry: u64) -> Result<(), String> {
    let sys = crate::port::Live::new();
    let ctx = crate::init::initial_setup_context(&sys)?;
    let [user] = ctx.users.as_slice() else {
        return Err("one console user is required".into());
    };
    let item = AttachedKeyboard {
        model: keyboard::Model { vendor, product },
        user: user.clone(),
        reading: LastReading::default(),
        expected: KeyboardCoverage::of([registry]),
    };
    let before = item.mappings(&sys)?;
    let targets: Vec<_> = before
        .iter()
        .filter(|map| map.registry == registry)
        .collect();
    if targets.len() != 1 {
        return Err("the exact keyboard service is absent or ambiguous".into());
    }
    let target = targets[0];
    let control = vec![(keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL)];
    if !target.native.is_empty() || target.user != control {
        return Err("native acceptance requires empty native mappings and existing user Caps→Left Control; nothing changed".into());
    }
    let change = KeyboardMapChange {
        registry,
        before_prefix: target.prefix.clone(),
        before_native: target.native.clone(),
        before_user: target.user.clone(),
        native: Some(control.clone()),
        user: None,
    };
    item.set_mappings(&sys, &[change])?;
    let after = item.mappings(&sys)?;
    if after.len() != before.len() {
        return Err("the keyboard service cohort changed during acceptance".into());
    }
    for old in before {
        let now: Vec<_> = after
            .iter()
            .filter(|map| map.registry == old.registry)
            .collect();
        if now.len() != 1
            || now[0].prefix != old.prefix
            || now[0].user != old.user
            || now[0].native
                != if old.registry == registry {
                    control.clone()
                } else {
                    old.native
                }
        {
            return Err(
                "native acceptance did not verify; an unrelated table or service changed".into(),
            );
        }
    }
    Ok(())
}

/// Exact final probe states admitted by attachment activation. A kept service records only
/// its unowned identity; it makes no claim about its Caps mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardAttachmentReceipt(Vec<AttachmentState>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum AttachmentState {
    Managed(KeyboardServiceState),
    Unmanaged(KeyboardServiceState),
    Kept(u64),
}

impl AttachmentState {
    fn registry(&self) -> u64 {
        match self {
            Self::Managed(state) | Self::Unmanaged(state) => state.registry,
            Self::Kept(registry) => *registry,
        }
    }
}

impl KeyboardAttachmentReceipt {
    pub(crate) fn empty() -> Self {
        Self(Vec::new())
    }

    /// Identities whose final state was verified, or explicitly left under a hold.
    #[must_use]
    pub fn registries(&self) -> Vec<u64> {
        self.0.iter().map(AttachmentState::registry).collect()
    }

    /// Compare a passive sample with the actual final probe. Unknown reads cannot match
    /// managed or intentionally unmanaged mappings; kept services remain unowned.
    #[must_use]
    pub fn matches(&self, observation: &KeyboardServiceObservation) -> bool {
        self.0.iter().any(|state| {
            state.registry() == observation.registry()
                && match state {
                    AttachmentState::Managed(state) | AttachmentState::Unmanaged(state) => {
                        observation.matches(state)
                    }
                    AttachmentState::Kept(_) => true,
                }
        })
    }

    /// Project only a complete, current passive cohort matching this actual final probe.
    /// Kept identities attest no mapping; an unknown, duplicate or missing service supplies
    /// no diagnostic. This never reads preferences or infers saved Caps readiness.
    #[must_use]
    pub fn live_mapping(
        &self,
        observations: &[KeyboardServiceObservation],
    ) -> Option<LiveKeyboardMapping> {
        if observations.is_empty()
            || observations.len() != self.0.len()
            || observations.len() > 4096
        {
            return None;
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = LiveKeyboardMapping {
            control: 0,
            explicit_other: 0,
            held: 0,
        };
        for observation in observations {
            if observation.unread().is_some() || !seen.insert(observation.registry()) {
                return None;
            }
            match self
                .0
                .iter()
                .find(|state| state.registry() == observation.registry())?
            {
                AttachmentState::Managed(state) | AttachmentState::Unmanaged(state) => {
                    if !observation.matches(state) {
                        return None;
                    }
                    if matches!(
                        state.caps_lock(),
                        keyboard::LEFT_CONTROL | keyboard::RIGHT_CONTROL
                    ) {
                        out.control += 1;
                    } else {
                        out.explicit_other += 1;
                    }
                }
                AttachmentState::Kept(_) => out.held += 1,
            }
        }
        Some(out)
    }

    /// Forget services that disappeared from the current cohort.
    pub fn retain(&mut self, registries: &[u64]) {
        self.0
            .retain(|state| registries.contains(&state.registry()));
    }

    /// Replace only services covered by a newer actual activation receipt.
    pub fn merge(&mut self, newer: Self) {
        let replaced = newer.registries();
        self.0.retain(|state| !replaced.contains(&state.registry()));
        self.0.extend(newer.0);
        self.0.sort_unstable_by_key(AttachmentState::registry);
    }
}

/// Activate the accepted Caps setting on only newly observed services, retaining a device's
/// explicit saved or live Caps choice. This never writes persistent preferences.
pub(crate) fn attachment_items(ctx: &Ctx, registries: &[u64]) -> Result<Vec<Attachment>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("input activation requires exactly one local user".into());
    };
    let mut models: Vec<_> = ctx
        .facts
        .keyboards
        .iter()
        .map(|keyboard| keyboard::Model {
            vendor: keyboard.vendor,
            product: keyboard.product,
        })
        .collect();
    models.sort();
    models.dedup();
    Ok(models
        .into_iter()
        .map(|model| Attachment {
            inner: AttachedKeyboard {
                model,
                user: user.clone(),
                reading: LastReading::default(),
                expected: KeyboardCoverage::default(),
            },
            registries: registries.to_vec(),
            observed: Mutex::default(),
            verified: Mutex::default(),
        })
        .collect())
}

/// One model's requested-service item and its most recent mapping coverage.
pub(crate) struct Attachment {
    inner: AttachedKeyboard,
    registries: Vec<u64>,
    observed: Mutex<KeyboardCoverage>,
    verified: Mutex<Option<KeyboardAttachmentReceipt>>,
}

impl Attachment {
    /// A held model needs only its matching identities, never its activation or defaults.
    pub(crate) fn inspect_kept(&self, sys: &dyn System) -> Result<KeyboardCoverage, String> {
        self.inner
            .matching_services(sys, Some(&self.registries))
            .map(|matched| KeyboardCoverage::of(matched.into_iter().map(|(id, _)| id)))
    }

    /// The latest probe or apply observation, never a previous successful cohort.
    pub(crate) fn observed_services(&self) -> KeyboardCoverage {
        self.observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn verified(&self) -> Option<KeyboardAttachmentReceipt> {
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn kept_receipt(&self, observed: &KeyboardCoverage) -> KeyboardAttachmentReceipt {
        KeyboardAttachmentReceipt(
            self.registries
                .iter()
                .copied()
                .filter(|registry| observed.covers(std::slice::from_ref(registry)))
                .map(AttachmentState::Kept)
                .collect(),
        )
    }

    fn maps(&self, sys: &dyn System) -> Result<(Vec<HidMapping>, Vec<HidMapping>), String> {
        *self.observed.lock().unwrap_or_else(PoisonError::into_inner) = KeyboardCoverage::default();
        *self.verified.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let maps = self.inner.mappings_for(sys, Some(&self.registries))?;
        *self.observed.lock().unwrap_or_else(PoisonError::into_inner) =
            KeyboardCoverage::of(maps.iter().map(|map| map.registry));
        if maps.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let target = Target::host("-g", &self.inner.model.key(), &self.inner.user);
        let accepted = match target.read(sys) {
            Read::Absent => true,
            Read::Unusable(why) => return Err(why),
            Read::Value(PValue::Array(pairs)) => {
                let mut mapping = Vec::new();
                for pair in pairs {
                    let (Some(PValue::Int(source)), Some(PValue::Int(destination))) =
                        (unique_field(&pair, SRC), unique_field(&pair, DST))
                    else {
                        return Err("unrecognized saved keyboard mapping; left untouched".into());
                    };
                    mapping.push((*source, *destination));
                }
                unique_sources(&mapping)?;
                mapping
                    .iter()
                    .find(|(source, _)| *source == keyboard::CAPS_LOCK)
                    .map_or(!mapping.is_empty(), |(_, destination)| {
                        matches!(
                            *destination,
                            keyboard::LEFT_CONTROL | keyboard::RIGHT_CONTROL
                        )
                    })
            }
            Read::Value(_) => {
                return Err("unrecognized saved keyboard mapping; left untouched".into());
            }
        };
        if !accepted {
            return Ok((Vec::new(), maps));
        }
        let (managed, unmanaged) = maps.into_iter().partition(|map| {
            matches!(
                map.caps_lock(),
                keyboard::LEFT_CONTROL | keyboard::RIGHT_CONTROL
            ) || !map
                .native
                .iter()
                .chain(&map.user)
                .any(|(source, _)| *source == keyboard::CAPS_LOCK)
        });
        Ok((managed, unmanaged))
    }
}

impl Item for Attachment {
    fn id(&self) -> String {
        self.inner.id()
    }
    fn family(&self) -> &'static str {
        self.inner.family()
    }
    fn title(&self) -> String {
        self.inner.title()
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let result = self.maps(sys).and_then(|(managed, unmanaged)| {
            let change = if managed.is_empty() {
                None
            } else {
                Some(merged_mapping(&managed)?)
            };
            if !matches!(change, Some(Some(_))) {
                let mut states: Vec<_> =
                    managed.into_iter().map(AttachmentState::Managed).collect();
                states.extend(unmanaged.into_iter().map(AttachmentState::Unmanaged));
                *self.verified.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(KeyboardAttachmentReceipt(states));
            }
            Ok(change)
        });
        match result {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::NotApplicable {
                why: "no new default keyboard service; explicit choices left untouched".into(),
            },
            Ok(Some(None)) => Probe::Ok {
                now: "Caps Lock acts as Control on new keyboard services".into(),
            },
            Ok(Some(Some(_))) => Probe::Change {
                from: "new keyboard service".into(),
                to: "Caps Lock acts as Control".into(),
            },
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if sys.euid() != self.inner.user.uid {
            return Err("keyboard activation must run as the target console user; existing remaps left untouched".into());
        }
        let (maps, _) = self.maps(sys)?;
        if !maps.is_empty()
            && let Some(changes) = merged_mapping(&maps)?
        {
            self.inner.set_mappings(sys, &changes)?;
        }
        Ok(())
    }
}

fn run(sys: &dyn System, cmd: &Cmd) -> Result<String, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    Ok(ran.out())
}

#[derive(Debug)]
struct AttachedKeyboard {
    model: keyboard::Model,
    user: User,
    /// Whether the last probe found Caps Lock unmapped, for System State.
    reading: LastReading,
    expected: KeyboardCoverage,
}

impl AttachedKeyboard {
    fn command(&self) -> Cmd {
        Cmd::new(HIDUTIL)
            .args([
                "property",
                "--matching",
                &crate::port::keyboard_usage::matching_model(self.model.vendor, self.model.product),
            ])
            .as_user(&self.user)
    }

    fn mappings(&self, sys: &dyn System) -> Result<Vec<HidMapping>, String> {
        let maps = self.mappings_for(sys, None)?;
        if !KeyboardCoverage::of(maps.iter().map(|map| map.registry)).covers_all(&self.expected) {
            return Err(
                "the live keyboard mapping read did not observe every assigned service".into(),
            );
        }
        Ok(maps)
    }

    fn mappings_for(
        &self,
        sys: &dyn System,
        registries: Option<&[u64]>,
    ) -> Result<Vec<HidMapping>, String> {
        let matched = self.matching_services(sys, registries)?;
        if matched.is_empty() {
            return Ok(Vec::new());
        }
        let command = Cmd::new(HIDUTIL)
            .args(["dump", "services", "-f", "xml"])
            .as_user(&self.user);
        let ran = sys
            .run_complete_stdout(&command)
            .map_err(|e| e.to_string())?;
        if !ran.success() {
            return Err(format!("{}: {}", command.display(), ran.describe()));
        }
        live_hid_mappings(&ran.out(), &matched)
    }

    fn matching_services(
        &self,
        sys: &dyn System,
        registries: Option<&[u64]>,
    ) -> Result<Vec<(u64, KeyMapping)>, String> {
        let command = self.command().args(["--get", "UserKeyMapping"]);
        let ran = sys
            .run_complete_stdout(&command)
            .map_err(|e| e.to_string())?;
        if !ran.success() {
            return Err(format!("{}: {}", command.display(), ran.describe()));
        }
        let mut matched = parse_hid_mappings(&ran.out())?;
        if let Some(registries) = registries {
            matched.retain(|(registry, _)| registries.contains(registry));
        }
        Ok(matched)
    }

    fn set_mappings(&self, sys: &dyn System, mappings: &[KeyboardMapChange]) -> Result<(), String> {
        // hidutil uses a Simple client, whose permitted properties exclude the native
        // modifier table. The Passive property backend can update it without privileges,
        // events or permission requests, and checks the exact services again before writing.
        sys.set_keyboard_maps(
            self.user.uid,
            self.model.vendor,
            self.model.product,
            mappings,
        )
    }
}

impl Item for AttachedKeyboard {
    fn id(&self) -> String {
        format!(
            "keyboard.attached.{}@{}",
            self.model.suffix(),
            self.user.name
        )
    }

    fn family(&self) -> &'static str {
        "keyboard"
    }

    fn title(&self) -> String {
        format!(
            "Caps Lock acts as Control now on keyboard {}",
            self.model.suffix()
        )
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.reading.set(None);
        let maps = match self.mappings(sys) {
            Ok(maps) => maps,
            Err(why) => return Probe::Unusable { why },
        };
        match merged_mapping(&maps) {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::Ok {
                now: "Caps Lock acts as Control on every attached service".into(),
            },
            Ok(Some(_)) => {
                // Every service leaves the physical key doing Caps Lock, including when
                // the two mapping tables compose back to Caps (`init_observe`).
                self.reading.set(
                    maps.iter()
                        .all(|map| map.caps_lock() == keyboard::CAPS_LOCK)
                        .then_some(Reading::CapsLock),
                );
                Probe::Change {
                    from: "Caps Lock is not Control on every attached service".into(),
                    to: "Caps Lock acts as Control immediately".into(),
                }
            }
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if sys.euid() != self.user.uid {
            return Err("keyboard activation must run as the target console user; existing remaps left untouched".into());
        }
        let maps = self.mappings(sys)?;
        if let Some(want) = merged_mapping(&maps)? {
            self.set_mappings(sys, &want)?;
        }
        Ok(())
    }
}

type KeyMapping = Vec<(i64, i64)>;

type HidMapping = KeyboardServiceState;

impl HidMapping {
    fn caps_lock(&self) -> i64 {
        self.output(keyboard::CAPS_LOCK)
    }

    fn output(&self, key: i64) -> i64 {
        let mapped = |map: &KeyMapping, key| {
            map.iter()
                .find_map(|(source, destination)| (*source == key).then_some(*destination))
                .unwrap_or(key)
        };
        // KeyboardPrefix proves every earlier Fn/modifier/NumLock layer passes Caps
        // through and no other key can enter Caps. The remaining native and user tables
        // each perform one lookup. Reading only the user table can falsely claim Control.
        mapped(&self.user, mapped(&self.native, key))
    }
}

fn merged_mapping(maps: &[HidMapping]) -> Result<Option<Vec<KeyboardMapChange>>, String> {
    if maps.is_empty() {
        return Err("no live HID service was observed for the attached keyboard".into());
    }
    let control = |key| matches!(key, keyboard::LEFT_CONTROL | keyboard::RIGHT_CONTROL);
    let mut changes = Vec::new();
    for map in maps {
        if control(map.caps_lock()) {
            continue;
        }
        let user_maps = |key| {
            map.user
                .iter()
                .find_map(|(source, destination)| (*source == key).then_some(*destination))
                .unwrap_or(key)
        };
        // Changing only native Caps preserves every other physical key's effective output,
        // even when a native Command→Caps feeds a user Caps mapping. Prefer an existing
        // user-table preimage of Control to adding a rule another physical key could enter.
        let preimage = [
            keyboard::LEFT_CONTROL,
            keyboard::RIGHT_CONTROL,
            keyboard::CAPS_LOCK,
        ]
        .into_iter()
        .chain(map.user.iter().map(|(source, _)| *source))
        .find(|key| control(user_maps(*key)));
        let mut user = None;
        let destination = if let Some(preimage) = preimage {
            preimage
        } else {
            if map.native.iter().any(|(source, destination)| {
                *source != keyboard::CAPS_LOCK && *destination == keyboard::CAPS_LOCK
            }) {
                return Err("existing modifier remaps conflict with Caps Lock as Control; unrelated keys left untouched".into());
            }
            let mut wanted: KeyMapping = map
                .user
                .iter()
                .copied()
                .filter(|(source, _)| *source != keyboard::CAPS_LOCK)
                .collect();
            wanted.push((keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL));
            user = Some(wanted);
            keyboard::CAPS_LOCK
        };
        let mut native: KeyMapping = map
            .native
            .iter()
            .copied()
            .filter(|(source, _)| *source != keyboard::CAPS_LOCK)
            .collect();
        if destination != keyboard::CAPS_LOCK {
            native.push((keyboard::CAPS_LOCK, destination));
        }
        changes.push(KeyboardMapChange {
            registry: map.registry,
            before_prefix: map.prefix.clone(),
            before_native: map.native.clone(),
            before_user: map.user.clone(),
            native: (native != map.native).then_some(native),
            user,
        });
    }
    Ok((!changes.is_empty()).then_some(changes))
}

fn parse_hid_mappings(text: &str) -> Result<Vec<(u64, KeyMapping)>, String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut registry = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("RegistryID") {
            continue;
        }
        if let Some((id, rest)) = line.split_once(char::is_whitespace)
            && !id.is_empty()
            && id.chars().all(|c| c.is_ascii_hexdigit())
            && let Some(rest) = rest.trim().strip_prefix("UserKeyMapping")
        {
            if !current.is_empty() {
                values.push((
                    registry.ok_or("missing keyboard service identity")?,
                    parse_mapping(&current)?,
                ));
                current.clear();
            }
            registry = Some(u64::from_str_radix(id, 16).map_err(|e| e.to_string())?);
            current.push_str(rest.trim());
        } else {
            current.push(' ');
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        values.push((
            registry.ok_or("missing keyboard service identity")?,
            parse_mapping(&current)?,
        ));
    }
    let mut ids: Vec<_> = values.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != values.len() {
        return Err("duplicate keyboard service identity; existing remaps left untouched".into());
    }
    Ok(values)
}

fn live_hid_mappings(text: &str, matched: &[(u64, KeyMapping)]) -> Result<Vec<HidMapping>, String> {
    if text.len() > aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES {
        return Err("keyboard service records exceed the complete-read bound".into());
    }
    let dump = crate::plist::parse(text.as_bytes()).map_err(|why| why.to_string())?;
    let records = unique_field(&dump, "ServiceRecords")
        .ok_or("missing or duplicate keyboard service records")?;
    matched
        .iter()
        .map(|(registry, user)| {
            let state = KeyboardServiceState::read(records, *registry)?;
            let mut expected = user.clone();
            expected.sort_unstable();
            if state.user != expected {
                return Err(
                    "keyboard mappings changed while reading; existing remaps left untouched"
                        .into(),
                );
            }
            Ok(state)
        })
        .collect()
}

fn unique_field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
    let PValue::Dict(fields) = value else {
        return None;
    };
    let mut values = fields.iter().filter(|(name, _)| name == key);
    let value = &values.next()?.1;
    values.next().is_none().then_some(value)
}

fn unique_sources(mappings: &KeyMapping) -> Result<(), String> {
    let mut sources: Vec<_> = mappings.iter().map(|(source, _)| *source).collect();
    sources.sort_unstable();
    sources.dedup();
    if sources.len() != mappings.len() {
        return Err("duplicate keyboard mapping source; existing remaps left untouched".into());
    }
    Ok(())
}

fn parse_mapping(value: &str) -> Result<Vec<(i64, i64)>, String> {
    if value.trim() == "(null)" {
        return Ok(Vec::new());
    }
    let mut tokens = Vec::new();
    let mut start = 0;
    for (at, ch) in value.char_indices() {
        if ch.is_ascii_whitespace() || "(){},;=".contains(ch) {
            if start < at {
                tokens.push(&value[start..at]);
            }
            if !ch.is_ascii_whitespace() {
                tokens.push(&value[at..at + ch.len_utf8()]);
            }
            start = at + ch.len_utf8();
        }
    }
    if start < value.len() {
        tokens.push(&value[start..]);
    }
    let invalid =
        || "unrecognized hidutil UserKeyMapping; existing remaps left untouched".to_owned();
    if tokens.first() != Some(&"(") || tokens.last() != Some(&")") {
        return Err(invalid());
    }
    let mut at = 1;
    let mut mappings = Vec::new();
    while at < tokens.len() - 1 {
        if tokens.get(at) != Some(&"{") {
            return Err(invalid());
        }
        at += 1;
        let (mut src, mut dst) = (None, None);
        for _ in 0..2 {
            let field = *tokens.get(at).ok_or_else(invalid)?;
            if tokens.get(at + 1) != Some(&"=") || tokens.get(at + 3) != Some(&";") {
                return Err(invalid());
            }
            let number = tokens
                .get(at + 2)
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?;
            match field {
                SRC if src.is_none() => src = Some(number),
                DST if dst.is_none() => dst = Some(number),
                _ => return Err(invalid()),
            }
            at += 4;
        }
        if tokens.get(at) != Some(&"}") {
            return Err(invalid());
        }
        mappings.push((src.ok_or_else(invalid)?, dst.ok_or_else(invalid)?));
        at += 1;
        if tokens.get(at) == Some(&",") {
            at += 1;
        } else if at != tokens.len() - 1 {
            return Err(invalid());
        }
    }
    unique_sources(&mappings)?;
    Ok(mappings)
}

/// A desktop item's kind: one `[consumer] widgets` catalog row's key, or a family of keys.
#[derive(Debug, Clone, Copy)]
enum DesktopKind {
    /// One widgets row of the catalog ([`desktop`]): its key, under its id.
    Widget(&'static CatalogRow),
    Desktop,
    Dock,
}

impl PartialEq for DesktopKind {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Widget(a), Self::Widget(b)) => a.id == b.id,
            (Self::Desktop, Self::Desktop) | (Self::Dock, Self::Dock) => true,
            _ => false,
        }
    }
}

impl Eq for DesktopKind {}

/// Which keys of its families a pass reads and writes ([`DesktopKind::reached`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reach {
    /// What the pass measured of its Full Disk Access: without a grant known held, no key
    /// in another app's container ([`defaults::withheld`]).
    full_disk_access: Option<bool>,
    /// A follow-up ([`withheld_items`]): only the keys in another app's container.
    contained_only: bool,
}

impl Reach {
    const fn all(full_disk_access: Option<bool>) -> Self {
        Self {
            full_disk_access,
            contained_only: false,
        }
    }

    fn reaches(self, domain: &str) -> bool {
        defaults::withheld(domain, self.full_disk_access).is_none()
            && (!self.contained_only || defaults::contained(domain))
    }
}

fn marker(kind: DesktopKind, domain: &str) -> &'static str {
    if defaults::contained(domain) {
        crate::init_activation::CONTAINED_MARKER
    } else if matches!(kind, DesktopKind::Widget(_)) {
        crate::init_activation::WIDGETS_MARKER
    } else {
        kind.family()
    }
}

/// What a desktop key should hold.
#[derive(Debug, Clone, Copy)]
enum Wanted {
    Bool(bool),
    /// An empty array: no stack, no recent app.
    Empty,
    /// The [`DOCK_APPS`] pins, aterm where it is installed ([`dock_apps_available`]), as the
    /// Dock rewrites them ([`same_apps`]).
    DockApps,
    /// Notification Center's widget list with no widget ([`widgets::cleared`]).
    NoWidgets,
    /// A catalog row's value ([`Lit::value`]).
    Value(Lit),
}

impl Wanted {
    const fn of(lit: Lit) -> Self {
        match lit {
            Lit::NoWidgets => Self::NoWidgets,
            lit => Self::Value(lit),
        }
    }
}

impl DesktopKind {
    const fn family(self) -> &'static str {
        match self {
            Self::Widget(_) => "widgets",
            Self::Desktop => "desktop",
            Self::Dock => "dock",
        }
    }

    fn name(self) -> String {
        match self {
            Self::Widget(row) => row.id.to_owned(),
            Self::Desktop | Self::Dock => format!("init.{}", self.family()),
        }
    }

    fn id(self, user: &User) -> String {
        format!("{}@{}", self.name(), user.name)
    }

    fn readers(self, reach: Reach, marker: &str) -> Vec<&'static str> {
        crate::init_activation::RELAUNCHED
            .into_iter()
            .filter(|process| {
                self.recorded(reach, marker)
                    .iter()
                    .any(|(domain, _, _)| crate::init_activation::readers(domain).contains(process))
            })
            .collect()
    }

    fn reached(self, reach: Reach) -> Vec<Key> {
        self.keys()
            .into_iter()
            .filter(|(domain, _, _)| reach.reaches(domain))
            .collect()
    }

    fn recorded(self, reach: Reach, marker: &str) -> Vec<Key> {
        self.reached(reach)
            .into_iter()
            .filter(|(domain, _, _)| self::marker(self, domain) == marker)
            .collect()
    }

    fn markers(self, reach: Reach) -> Vec<&'static str> {
        let mut markers = Vec::new();
        for (domain, _, _) in self.reached(reach) {
            let marker = marker(self, domain);
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
        markers
    }

    fn withheld(self, full_disk_access: Option<bool>) -> Vec<String> {
        let mut why: Vec<String> = Vec::new();
        for (domain, _, _) in self.keys() {
            if let Some(line) = defaults::withheld(domain, full_disk_access)
                && !why.contains(&line)
            {
                why.push(line);
            }
        }
        why
    }

    fn keys(self) -> Vec<Key> {
        let keys: &[Key] = match self {
            Self::Widget(row) => return vec![(row.domain, row.key, Wanted::of(row.off))],
            Self::Desktop => &[
                (
                    WINDOW_MANAGER,
                    "EnableStandardClickToShowDesktop",
                    Wanted::Bool(false),
                ),
                (
                    WINDOW_MANAGER,
                    "StandardHideDesktopIcons",
                    Wanted::Bool(false),
                ),
                (
                    WINDOW_MANAGER,
                    "StageManagerHideDesktopIcons",
                    Wanted::Bool(false),
                ),
                (FINDER, "CreateDesktop", Wanted::Bool(true)),
            ],
            Self::Dock => &[
                (DOCK, "autohide", Wanted::Bool(true)),
                (DOCK, "persistent-apps", Wanted::DockApps),
                (DOCK, "persistent-others", Wanted::Empty),
                (DOCK, "static-apps", Wanted::Empty),
                (DOCK, "static-others", Wanted::Empty),
                (DOCK, "recent-apps", Wanted::Empty),
                (DOCK, "show-recents", Wanted::Bool(false)),
                (DOCK, "static-only", Wanted::Bool(false)),
                (DOCK, "minimize-to-application", Wanted::Bool(true)),
            ],
        };
        keys.to_vec()
    }
}

type Key = (&'static str, &'static str, Wanted);

#[derive(Debug)]
struct Desktop {
    user: User,
    kind: DesktopKind,
    before: Mutex<Vec<(Target, Read<PValue>)>>,
    /// What the last probe found to change, which the apply after it writes (the engine
    /// applies only right after a probe that found a change), so the apply reads nothing again.
    planned: Mutex<Option<Changes>>,
    /// The pass's one relaunch of what the desktop items write ([`DesktopActivation`]).
    activation: Arc<DesktopActivation>,
    /// What the last probe read, for System State.
    reading: LastReading,
}

type Changes = Vec<(Target, Read<PValue>, PValue)>;

impl DesktopKind {
    /// What the pending changes read, for System State's words, from the values themselves: an
    /// absent key holds macOS's default, which is not read.
    fn reading(self, changes: &Changes) -> Option<Reading> {
        let reads = |key: &str, value: bool| {
            changes.iter().any(|(target, read, _)| {
                target.key == key
                    && matches!(read, Read::Value(now) if now.same(&PValue::Bool(value)))
            })
        };
        match self {
            Self::Desktop => {
                if reads("CreateDesktop", false)
                    || reads("StandardHideDesktopIcons", true)
                    || reads("StageManagerHideDesktopIcons", true)
                {
                    Some(Reading::IconsHidden)
                } else if reads("EnableStandardClickToShowDesktop", true) {
                    Some(Reading::ClickReveals)
                } else {
                    None
                }
            }
            Self::Widget(_) => {
                let listed = changes.iter().any(|(target, read, _)| {
                    target.key == "widgets"
                        && matches!(read, Read::Value(PValue::Dict(fields))
                            if fields.iter().any(|(key, value)| key == "instances"
                                && matches!(value, PValue::Array(list) if !list.is_empty())))
                });
                (listed
                    || reads("StandardHideWidgets", false)
                    || reads("StageManagerHideWidgets", false))
                .then_some(Reading::WidgetsShown)
            }
            Self::Dock => changes
                .iter()
                .find_map(|(target, read, want)| match (read, want) {
                    (Read::Value(PValue::Array(pins)), PValue::Array(wanted))
                        if target.key == "persistent-apps" =>
                    {
                        Some(Reading::Apps {
                            pinned: pins.len(),
                            wanted: wanted.len(),
                        })
                    }
                    _ => None,
                }),
        }
    }
}

impl Desktop {
    fn new(user: &User, kind: DesktopKind, activation: &Arc<DesktopActivation>) -> Self {
        Self {
            user: user.clone(),
            kind,
            before: Mutex::new(Vec::new()),
            planned: Mutex::new(None),
            activation: Arc::clone(activation),
            reading: LastReading::default(),
        }
    }

    fn target(&self, domain: &str, key: &str) -> Target {
        Target::user(domain, key, &self.user)
    }

    fn reach(&self) -> Reach {
        self.activation.reach
    }

    fn full_disk_access(&self) -> Option<bool> {
        self.reach().full_disk_access
    }

    fn pending(&self, sys: &dyn System) -> Result<bool, String> {
        for marker in self
            .kind
            .markers(self.reach())
            .into_iter()
            .chain(self.legacy())
        {
            if self.activation.earlier(sys, marker)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn legacy(&self) -> Option<&'static str> {
        let wm = matches!(self.kind, DesktopKind::Widget(row) if !defaults::contained(row.domain));
        (wm && !self.reach().contained_only)
            .then_some(crate::init_activation::LEGACY_WIDGETS_MARKER)
    }

    fn carry_legacy(&self, sys: &dyn System) -> Result<(), String> {
        let Some(legacy) = self.legacy() else {
            return Ok(());
        };
        if !crate::init_activation::pending(sys, &self.user, legacy)? {
            return Ok(());
        }
        for marker in [
            crate::init_activation::WIDGETS_MARKER,
            crate::init_activation::CONTAINED_MARKER,
        ] {
            crate::init_activation::begin(sys, &self.user, marker)?;
            self.activation.carried(marker);
        }
        crate::init_activation::finish(sys, &self.user, legacy)
    }

    fn summary(&self) -> String {
        let withheld = self.kind.withheld(self.full_disk_access());
        if withheld.is_empty() {
            self.title()
        } else {
            format!("{} ({})", self.title(), withheld.join("; "))
        }
    }

    /// What this item's keys hold now and what each that differs is to become: one reading,
    /// each domain read once ([`defaults::Exports`]).
    fn changes(&self, sys: &dyn System) -> Result<Changes, String> {
        let apps = if self.kind == DesktopKind::Dock {
            dock_apps_available(sys, &self.user)?
        } else {
            Vec::new()
        };
        let mut exports = defaults::Exports::default();
        let mut changes = Vec::new();
        for (domain, key, wanted) in self.kind.reached(self.reach()) {
            let target = self.target(domain, key);
            let read = exports.read(sys, &target);
            let want = match wanted {
                Wanted::NoWidgets => widgets::cleared(sys, &self.user, &read)?,
                Wanted::DockApps => unmet(&read, dock_apps(&apps), |now, want| {
                    same_apps(sys, now, want)
                })?,
                Wanted::Empty => unmet(&read, PValue::Array(Vec::new()), equivalent)?,
                Wanted::Bool(value) => unmet(&read, PValue::Bool(value), equivalent)?,
                Wanted::Value(value) => unmet(&read, value.value(), equivalent)?,
            };
            if let Some(want) = want {
                changes.push((target, read, want));
            }
        }
        Ok(changes)
    }
}

fn unmet(
    read: &Read<PValue>,
    want: PValue,
    same: impl Fn(&PValue, &PValue) -> bool,
) -> Result<Option<PValue>, String> {
    match read {
        Read::Unusable(why) => Err(why.clone()),
        Read::Value(now) if same(now, &want) => Ok(None),
        Read::Absent | Read::Value(_) => Ok(Some(want)),
    }
}

/// The one relaunch, per pass, of the desktop processes that read what that pass's
/// widget, desktop and Dock items wrote: the Dock, the Finder and Notification Center each
/// relaunch once, after the last of those writes, and the items are read back only once
/// each is up again and past its start ([`crate::init_activation::relaunch_settled`]).
#[derive(Debug)]
struct DesktopActivation {
    user: User,
    /// Which keys its families reach ([`DesktopKind::reached`]), from the Full Disk Access
    /// the pass measured, and so which markers, readers and looks are the pass's.
    reach: Reach,
    /// The families whose items started writing in this pass, in run order.
    owed: Mutex<Vec<DesktopKind>>,
    /// Each marker the pass's items looked at, and whether it was pending at the first
    /// look, until the pass's relaunch discharges it ([`DesktopActivation::earlier`]).
    seen: Mutex<Vec<(&'static str, bool)>>,
    /// Each marker an item of the pass began ([`DesktopActivation::begin`]), until the pass's
    /// relaunch discharges it: another item of the family writes behind the same marker.
    begun: Mutex<Vec<&'static str>>,
}

type Obligation = (DesktopKind, &'static str);

impl DesktopActivation {
    fn new(user: &User, reach: Reach) -> Self {
        Self {
            user: user.clone(),
            reach,
            owed: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
            begun: Mutex::new(Vec::new()),
        }
    }

    /// Begin `marker` ([`crate::init_activation::begin`]) once per pass: an item of the pass
    /// that began it already put it on disk, ordered before that item's writes and so before
    /// this one's.
    fn begin(&self, sys: &dyn System, marker: &'static str) -> Result<(), String> {
        let mut begun = self.begun.lock().unwrap_or_else(PoisonError::into_inner);
        if !begun.contains(&marker) {
            crate::init_activation::begin(sys, &self.user, marker)?;
            begun.push(marker);
        }
        Ok(())
    }

    fn earlier(&self, sys: &dyn System, marker: &'static str) -> Result<bool, String> {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        match seen.iter().find(|(looked, _)| *looked == marker) {
            Some(&(_, false)) => Ok(false),
            Some(&(_, true)) => crate::init_activation::pending(sys, &self.user, marker),
            None => {
                let pending = crate::init_activation::pending(sys, &self.user, marker)?;
                seen.push((marker, pending));
                Ok(pending)
            }
        }
    }

    fn carried(&self, marker: &'static str) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.retain(|(looked, _)| *looked != marker);
        seen.push((marker, true));
    }

    fn owe(&self, kind: DesktopKind) {
        let mut owed = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
        if !owed.contains(&kind) {
            owed.push(kind);
        }
    }

    fn obligations(&self, sys: &dyn System) -> Result<Vec<Obligation>, String> {
        let owed = self
            .owed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut due = Vec::new();
        for kind in owed {
            for marker in kind.markers(self.reach) {
                if crate::init_activation::pending(sys, &self.user, marker)? {
                    due.push((kind, marker));
                }
            }
        }
        Ok(due)
    }

    fn consumers(&self, due: &[Obligation]) -> Vec<&'static str> {
        crate::init_activation::RELAUNCHED
            .into_iter()
            .filter(|process| {
                due.iter()
                    .any(|(kind, marker)| kind.readers(self.reach, marker).contains(process))
            })
            .collect()
    }

    fn look(
        &self,
        sys: &dyn System,
        due: &[Obligation],
        process: &str,
    ) -> Result<Vec<Result<Entries, String>>, String> {
        let keys: Vec<Target> = due
            .iter()
            .flat_map(|&(kind, marker)| kind.recorded(self.reach, marker))
            .filter(|(domain, _, _)| crate::init_activation::readers(domain).contains(&process))
            .map(|(domain, key, _)| Target::user(domain, key, &self.user))
            .collect();
        crate::init_activation::reading(sys, &keys)
    }
}

fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

impl Activation for DesktopActivation {
    fn id(&self) -> String {
        format!("init.relaunch@{}", self.user.name)
    }

    fn pending(&self, sys: &dyn System, _ctx: &Ctx) -> Result<Option<(String, String)>, String> {
        let due = self.obligations(sys)?;
        if due.is_empty() {
            return Ok(None);
        }
        let processes: Vec<String> = self
            .consumers(&due)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut writers: Vec<String> = Vec::new();
        for (kind, _) in &due {
            let id = kind.id(&self.user);
            if !writers.contains(&id) {
                writers.push(id);
            }
        }
        let one = processes.len() == 1;
        Ok(Some((
            format!(
                "{} running with the preferences {} read at {} start",
                listed(&processes),
                if one { "it" } else { "they" },
                if one { "its" } else { "their" },
            ),
            format!(
                "{} relaunched once, after {} wrote, and up again",
                listed(&processes),
                listed(&writers),
            ),
        )))
    }

    fn activate(&self, sys: &dyn System, ctx: &Ctx) -> Result<Vec<(String, String)>, String> {
        let due = self.obligations(sys)?;
        let processes = self.consumers(&due);
        let answers = crate::init_activation::relaunch_settled(
            sys,
            &self.user,
            defaults::Session::of(&self.user, ctx),
            &processes,
            |process| self.look(sys, &due, process),
        );
        let mut failed: Vec<(String, String)> = Vec::new();
        for &(kind, marker) in &due {
            let readers = kind.readers(self.reach, marker);
            let why: Vec<&str> = processes
                .iter()
                .zip(&answers)
                .filter(|(process, _)| readers.contains(process))
                .filter_map(|(_, answer)| answer.as_ref().err().map(String::as_str))
                .collect();
            let finished = if why.is_empty() {
                crate::init_activation::finish(sys, &self.user, marker)
            } else {
                Err(why.join("; "))
            };
            if let Err(why) = finished {
                let id = kind.id(&self.user);
                match failed.iter_mut().find(|(failed, _)| *failed == id) {
                    Some((_, all)) if !all.contains(&why) => {
                        all.push_str("; ");
                        all.push_str(&why);
                    }
                    Some(_) => {}
                    None => failed.push((id, why)),
                }
            }
        }
        self.owed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|kind| !due.iter().any(|(owed, _)| owed == kind));
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(looked, _)| !due.iter().any(|(_, marker)| marker == looked));
        self.begun
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|begun| !due.iter().any(|(_, marker)| marker == begun));
        Ok(failed)
    }
}

impl Item for Desktop {
    fn preference_changes(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        Ok(self
            .changes(sys)?
            .into_iter()
            .map(|(target, read, _)| (target, read))
            .collect())
    }
    fn id(&self) -> String {
        self.kind.id(&self.user)
    }

    fn family(&self) -> &'static str {
        self.kind.family()
    }

    fn title(&self) -> String {
        match self.kind {
            DesktopKind::Widget(row)
                if !self.reach().contained_only
                    && !self.kind.withheld(self.full_disk_access()).is_empty() =>
            {
                format!("No {}; they wait for Full Disk Access", row.what)
            }
            DesktopKind::Widget(row) => format!("No {}", row.what),
            DesktopKind::Desktop => {
                "Desktop icons visible; clicking wallpaper keeps windows in place".into()
            }
            DesktopKind::Dock => {
                "Dock auto-hides; pins: aterm, System Settings, Safari; no stacks or recent apps"
                    .into()
            }
        }
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        self.reading.set(None);
        *self.planned.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let pending = match self.pending(sys) {
            Ok(pending) => pending,
            Err(why) => return Probe::Unusable { why },
        };
        match self.changes(sys) {
            Err(why) => Probe::Unusable { why },
            Ok(changes) if changes.is_empty() && !pending => Probe::Ok {
                now: self.summary(),
            },
            Ok(changes) => {
                self.reading.set(self.kind.reading(&changes));
                let from = if changes.is_empty() {
                    "stored preferences await activation after an earlier write".into()
                } else {
                    changes
                        .iter()
                        .map(|(target, _, _)| target.describe())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                *self.before.lock().unwrap_or_else(PoisonError::into_inner) = changes
                    .iter()
                    .map(|(target, read, _)| (target.clone(), read.clone()))
                    .collect();
                *self.planned.lock().unwrap_or_else(PoisonError::into_inner) = Some(changes);
                if ctx.disrupt {
                    Probe::Change {
                        from,
                        to: self.summary(),
                    }
                } else {
                    Probe::Deferred { why: "relaunches the user's desktop process; use the local init command at the console".into() }
                }
            }
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.carry_legacy(sys)?;
        let planned = self
            .planned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let changes = match planned {
            Some(changes) => changes,
            None => self.changes(sys)?,
        };
        let mut owed = false;
        for marker in self.kind.markers(self.reach()) {
            let writes = changes
                .iter()
                .any(|(target, _, _)| self::marker(self.kind, &target.domain) == marker);
            if writes || crate::init_activation::pending(sys, &self.user, marker)? {
                self.activation.begin(sys, marker)?;
                owed = true;
            }
        }
        if !owed {
            return Ok(());
        }
        self.activation.owe(self.kind);
        for (target, _, want) in changes {
            run(sys, &target.write_cmd(&want))?;
        }
        Ok(())
    }

    fn activation(&self) -> Option<Arc<dyn Activation>> {
        Some(Arc::clone(&self.activation) as Arc<dyn Activation>)
    }

    fn effect(&self) -> Effect {
        Effect::Relaunch
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    fn withheld(&self) -> Option<String> {
        let why = self.kind.withheld(self.full_disk_access());
        (!why.is_empty()).then(|| why.join("; "))
    }

    fn revert(&self) -> Option<String> {
        let before = self.before.lock().unwrap_or_else(PoisonError::into_inner);
        let lines: Vec<_> = before
            .iter()
            .filter_map(|(target, read)| defaults::revert_line(target, read, None))
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

fn equivalent(now: &PValue, want: &PValue) -> bool {
    now.same(want)
}

fn field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
    let PValue::Dict(entries) = value else {
        return None;
    };
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn dock_apps(apps: &[DockApp]) -> PValue {
    PValue::Array(
        apps.iter()
            .map(|&(ref path, bundle, label)| {
                PValue::Dict(vec![
                    ("tile-type".into(), PValue::Str("file-tile".into())),
                    (
                        "tile-data".into(),
                        PValue::Dict(vec![
                            ("bundle-identifier".into(), PValue::Str(bundle.into())),
                            ("file-label".into(), PValue::Str(label.into())),
                            (
                                "file-data".into(),
                                PValue::Dict(vec![
                                    ("_CFURLString".into(), PValue::Str(file_url(path))),
                                    ("_CFURLStringType".into(), PValue::Int(15)),
                                ]),
                            ),
                        ]),
                    ),
                ])
            })
            .collect(),
    )
}

fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for &byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&byte) {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url.push('/');
    url
}

type DockApp = (String, &'static str, &'static str);

fn aterm_paths(user: &User) -> [String; 2] {
    [
        DOCK_APPS[0].0.to_owned(),
        format!("{}/Applications/aterm.app", user.home.trim_end_matches('/')),
    ]
}

/// Where aterm may be pinned from, in order: the bundle this aterm runs from, under the
/// spelling of the usual path ([`aterm_paths`]) it is when it is one (wherever a link there
/// leads), else where it is when that copy lasts ([`aterm_toolchain::lasting`]); then the usual
/// paths. With the running copy when it was passed over for not lasting (a disk image, an App
/// Translocation copy, a temporary or cache directory).
fn aterm_candidates(sys: &dyn System, user: &User) -> (Vec<String>, Option<PathBuf>) {
    let mut paths = aterm_paths(user).to_vec();
    let Some(running) = sys.running_app() else {
        return (paths, None);
    };
    let canonical = |path: &str| {
        sys.canonicalize(Path::new(path))
            .unwrap_or_else(|_| path.into())
    };
    let own = match paths.iter().position(|path| canonical(path) == running) {
        Some(at) => paths.remove(at),
        None if aterm_toolchain::lasting::lasting(&running) => {
            running.to_string_lossy().into_owned()
        }
        None => return (paths, Some(running)),
    };
    paths.insert(0, own);
    (paths, None)
}

/// The [`DOCK_APPS`] as installed, aterm at the first of [`aterm_candidates`] that holds it,
/// resolved at each reading so the pin follows the app when it moves.
/// # Errors
/// An app is missing, unreadable, or another app: the Dock is left untouched.
fn dock_apps_available(sys: &dyn System, user: &User) -> Result<Vec<DockApp>, String> {
    let [(_, aterm, label), others @ ..] = DOCK_APPS;
    let mut apps = Vec::with_capacity(DOCK_APPS.len());
    let mut why = Vec::new();
    let (candidates, fleeting) = aterm_candidates(sys, user);
    for path in candidates {
        match installed_app(sys, user, &path, aterm, label) {
            Ok(()) => {
                apps.push((path, aterm, label));
                break;
            }
            Err(Missing::NotInstalled) => why.push((true, format!("nothing at {path}"))),
            Err(Missing::Other(line)) => why.push((false, line)),
        }
    }
    if apps.is_empty() {
        let [usual, own] = aterm_paths(user);
        let running = fleeting.map_or_else(String::new, |copy| {
            format!(
                "; it runs from {}, a copy that goes away (a disk image or App Translocation \
                 copy): move aterm.app to /Applications",
                copy.display()
            )
        });
        return Err(if why.iter().all(|(missing, _)| *missing) {
            format!("{label} is not installed at {usual} or {own}{running}; Dock left untouched")
        } else {
            let why: Vec<String> = why.into_iter().map(|(_, line)| line).collect();
            format!(
                "no {label} to pin ({}){running}; Dock left untouched",
                why.join(", ")
            )
        });
    }
    for (path, bundle, label) in others {
        installed_app(sys, user, path, bundle, label).map_err(|missing| {
            let why = match missing {
                Missing::NotInstalled => format!("{label} is not installed at {path}"),
                Missing::Other(line) => line,
            };
            format!("{why}; Dock left untouched")
        })?;
    }
    apps.extend(
        others
            .into_iter()
            .map(|(path, bundle, label)| (path.to_owned(), bundle, label)),
    );
    Ok(apps)
}

enum Missing {
    /// Nothing is at its path.
    NotInstalled,
    /// Its bundle cannot be read, or is another app; the line says which.
    Other(String),
}

fn installed_app(
    sys: &dyn System,
    user: &User,
    path: &str,
    bundle: &str,
    label: &str,
) -> Result<(), Missing> {
    let contents = Path::new(path).join("Contents");
    if !sys.stat(&contents).is_ok_and(|s| s.dir) {
        return Err(Missing::NotInstalled);
    }
    let cmd = Cmd::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-", "--"])
        .arg(contents.join("Info.plist"))
        .as_user(user);
    let ran = sys
        .run_complete_stdout(&cmd)
        .map_err(|_| Missing::Other(format!("cannot verify {label} at {path}")))?;
    if !ran.success() || ran.out().trim() != bundle {
        return Err(Missing::Other(format!(
            "{path} is not the requested {label} application"
        )));
    }
    Ok(())
}

fn same_apps(sys: &dyn System, now: &PValue, want: &PValue) -> bool {
    let (PValue::Array(now), PValue::Array(want)) = (now, want) else {
        return false;
    };
    now.len() == want.len()
        && now.iter().zip(want).all(|(now, want)| {
            field(now, "tile-type") == field(want, "tile-type")
                && field(now, "tile-data").and_then(|v| field(v, "bundle-identifier"))
                    == field(want, "tile-data").and_then(|v| field(v, "bundle-identifier"))
                && dock_app_path(now)
                    .zip(dock_app_path(want))
                    .is_some_and(|(now, want)| {
                        sys.canonicalize(&now).unwrap_or(now)
                            == sys.canonicalize(&want).unwrap_or(want)
                    })
        })
}

fn dock_app_path(pin: &PValue) -> Option<PathBuf> {
    let file = field(field(pin, "tile-data")?, "file-data")?;
    let PValue::Str(value) = field(file, "_CFURLString")? else {
        return None;
    };
    let path = match field(file, "_CFURLStringType")? {
        PValue::Int(0) => PathBuf::from(value),
        PValue::Int(15) => {
            let url = value.strip_prefix("file://")?;
            let (authority, encoded) = url.split_once('/')?;
            if !matches!(authority, "" | "localhost") || encoded.contains(['?', '#']) {
                return None;
            }
            let mut decoded = vec![b'/'];
            let mut bytes = encoded.as_bytes().iter().copied();
            while let Some(byte) = bytes.next() {
                decoded.push(if byte == b'%' {
                    let high = char::from(bytes.next()?).to_digit(16)?;
                    let low = char::from(bytes.next()?).to_digit(16)?;
                    u8::try_from(high * 16 + low).ok()?
                } else {
                    byte
                });
            }
            PathBuf::from(String::from_utf8(decoded).ok()?)
        }
        _ => return None,
    };
    (path.is_absolute()
        && !path.components().any(|c| c == Component::ParentDir)
        && !path.as_os_str().as_encoded_bytes().contains(&0))
    .then_some(path)
}

#[cfg(test)]
#[path = "init_preferences_tests.rs"]
pub(crate) mod tests;

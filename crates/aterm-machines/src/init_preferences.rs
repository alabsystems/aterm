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
pub(crate) mod tests {
    use super::*;
    use crate::init::Workstation;
    use crate::items::defaults::tests::{ctx, export, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    const WIDGETS: &str = crate::init_activation::WIDGETS_MARKER;

    const NOTIFICATION_CENTER: &str = "com.apple.notificationcenterui";

    fn desktop(who: &User, kind: DesktopKind) -> Desktop {
        Desktop::new(
            who,
            kind,
            &Arc::new(DesktopActivation::new(who, Reach::all(Some(true)))),
        )
    }

    fn usual_dock_apps() -> Vec<DockApp> {
        DOCK_APPS
            .into_iter()
            .map(|(path, bundle, label)| (path.to_owned(), bundle, label))
            .collect()
    }

    pub(crate) fn installed_dock_apps(sys: &Fake, who: &User) {
        for (path, bundle, _) in DOCK_APPS {
            installed_at(sys, who, path, bundle);
        }
    }

    fn installed_at(sys: &Fake, who: &User, path: &str, bundle: &str) {
        sys.stat_of(
            &format!("{path}/Contents"),
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o755,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        let cmd = Cmd::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-", "--"])
            .arg(format!("{path}/Contents/Info.plist"))
            .as_user(who);
        sys.on(&cmd.display(), Ran::ok(bundle));
    }

    #[test]
    fn hidutil_table_keeps_services_distinct_and_preserves_their_existing_user_rules() {
        let table = "RegistryID Key Value\n100000001 UserKeyMapping (\n{ HIDKeyboardModifierMappingDst = 30064771297; HIDKeyboardModifierMappingSrc = 30064771303; },\n{ HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771241; }\n)\n100000002 UserKeyMapping (null)\n";
        let maps: Vec<_> = parse_hid_mappings(table)
            .unwrap()
            .into_iter()
            .map(|(registry, user)| HidMapping {
                registry,
                prefix: KeyboardPrefix::default(),
                native: Vec::new(),
                user,
            })
            .collect();
        assert_eq!(maps.len(), 2);
        let changes = merged_mapping(&maps).unwrap().unwrap();
        assert_eq!(changes.len(), 2);
        for (before, change) in maps.into_iter().zip(changes) {
            assert_eq!(change.registry, before.registry);
            assert_eq!(change.before_user, before.user);
            assert_eq!(change.user, None);
            let after = after_change(before, change);
            assert_eq!(after.caps_lock(), keyboard::LEFT_CONTROL);
            assert_eq!(merged_mapping(&[after]).unwrap(), None);
        }
        assert!(parse_hid_mappings("100000001 UserKeyMapping (garbage)").is_err());
    }

    fn after_change(before: HidMapping, change: KeyboardMapChange) -> HidMapping {
        HidMapping {
            registry: before.registry,
            prefix: before.prefix,
            native: change.native.unwrap_or(before.native),
            user: change.user.unwrap_or(before.user),
        }
    }

    pub(crate) type HidDumpRow<'a> = (u64, &'a [(i64, i64)], &'a [(i64, i64)]);

    pub(crate) fn hid_dump(maps: &[HidDumpRow<'_>]) -> String {
        hid_dump_with_prefix(maps, &crate::port::keyboard_prefix::tests::fields())
    }

    fn hid_dump_with_prefix(maps: &[HidDumpRow<'_>], prefix: &[(String, PValue)]) -> String {
        let pairs = |map: &[(i64, i64)]| {
            PValue::Array(
                map.iter()
                    .map(|(source, destination)| {
                        PValue::Dict(vec![
                            ("Src".into(), PValue::Int(*source)),
                            ("Dst".into(), PValue::Int(*destination)),
                        ])
                    })
                    .collect(),
            )
        };
        crate::plist::document(&PValue::Dict(vec![(
            "ServiceRecords".into(),
            PValue::Array(
                maps.iter()
                    .map(|(registry, native, user)| {
                        PValue::Dict(vec![
                            (
                                "IORegistryEntryID".into(),
                                PValue::Int(i64::try_from(*registry).unwrap()),
                            ),
                            ("PrimaryUsagePage".into(), PValue::Int(1)),
                            ("PrimaryUsage".into(), PValue::Int(6)),
                            (
                                "ServiceFilterDebug".into(),
                                PValue::Array(vec![PValue::Dict(vec![(
                                    "plugin".into(),
                                    PValue::Dict(
                                        [
                                            (
                                                "Class".into(),
                                                PValue::Str("IOHIDKeyboardFilter".into()),
                                            ),
                                            (NATIVE_MAPPING.into(), pairs(native)),
                                            ("UserKeyMapping".into(), pairs(user)),
                                        ]
                                        .into_iter()
                                        .chain(prefix.iter().cloned())
                                        .collect(),
                                    ),
                                )])]),
                            ),
                        ])
                    })
                    .collect(),
            ),
        )]))
    }

    fn hid_dump_command(who: &User) -> Cmd {
        Cmd::new(HIDUTIL)
            .args(["dump", "services", "-f", "xml"])
            .as_user(who)
    }

    fn attachment(who: &User, vendor: u32, product: u32) -> Attachment {
        Attachment {
            inner: AttachedKeyboard {
                model: keyboard::Model { vendor, product },
                user: who.clone(),
                reading: LastReading::default(),
                expected: KeyboardCoverage::default(),
            },
            registries: vec![2],
            observed: Mutex::default(),
            verified: Mutex::default(),
        }
    }

    fn hid_user_row(registry: u64, pairs: &[(i64, i64)]) -> String {
        let map = if pairs.is_empty() {
            "(null)".into()
        } else {
            format!(
                "( {} )",
                pairs
                    .iter()
                    .map(|(src, dst)| format!("{{ {SRC} = {src}; {DST} = {dst}; }}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        format!("RegistryID Key Value\n{registry:x} UserKeyMapping {map}\n")
    }

    #[test]
    fn automatic_attachment_targets_only_new_services_and_preserves_unrelated_remaps() {
        let who = user("alice", 501);
        let item = attachment(&who, 65534, 169);
        let mut sys = Fake::root();
        sys.euid = who.uid;
        let option = 0x7_0000_00e2;
        let command = 0x7_0000_00e3;
        let native = [(option, command)];
        let user_map = [(keyboard::LEFT_CONTROL, option)];
        sys.on(
            &item
                .inner
                .command()
                .args(["--get", "UserKeyMapping"])
                .display(),
            Ran::ok(&hid_user_row(2, &user_map)),
        );
        sys.on(
            &hid_dump_command(&who).display(),
            Ran::ok(&composite_keyboard_dump(
                &[
                    (1, &[(keyboard::CAPS_LOCK, option)], &[]),
                    (2, &native, &user_map),
                ],
                true,
            )),
        );
        sys.on(
            &Target::host("-g", &item.inner.model.key(), &who)
                .export_cmd()
                .display(),
            export("<dict/>"),
        );
        let context = ctx(std::slice::from_ref(&who));
        assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
        assert!(item.observed_services().covers(&[2]));
        assert!(item.verified().is_none(), "a before-image is not a receipt");
        item.apply(&sys, &context).unwrap();
        assert!(item.verified().is_none(), "a setter is not its read-back");
        let changes = sys.keyboard_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].registry, 2);
        assert_eq!(changes[0].user, None);
        assert_eq!(
            changes[0].native,
            Some(vec![
                (option, command),
                (keyboard::CAPS_LOCK, keyboard::RIGHT_CONTROL)
            ])
        );
        let other = attachment(&who, 1452, 999);
        sys.on(
            &other
                .inner
                .command()
                .args(["--get", "UserKeyMapping"])
                .display(),
            Ran::ok("RegistryID Key Value\n3 UserKeyMapping (null)\n"),
        );
        let before = sys.calls().len();
        assert!(matches!(
            other.probe(&sys, &context),
            Probe::NotApplicable { .. }
        ));
        assert!(!other.observed_services().covers(&[2]));
        assert_eq!(
            sys.calls().len(),
            before + 1,
            "another model reads no preference or live filter when it owns no new service"
        );
        assert_eq!(sys.keyboard_changes().len(), 1);

        // The final successful probe, not the pre-apply observation or a later passive
        // read, supplies the receipt. Passive reads reuse this exact modeled data.
        sys.logged_in(&who);
        sys.keyboard_services = Some(Ok(vec![2]));
        let verified_native = changes[0].native.clone().unwrap();
        let final_dump =
            composite_keyboard_dump(&[(1, &[], &[]), (2, &verified_native, &user_map)], true);
        sys.simulate(move |_, cmd| {
            (cmd.args.first().and_then(|arg| arg.to_str()) == Some("dump"))
                .then(|| Ran::ok(&final_dump))
        });
        assert!(matches!(item.probe(&sys, &context), Probe::Ok { .. }));
        let receipt = item.verified().unwrap();
        let calls = sys.calls();
        let observed = sys.keyboard_service_observations().unwrap();
        assert_eq!(receipt.registries(), [2]);
        assert!(receipt.matches(&observed[0]));
        assert_eq!(
            receipt.live_mapping(&observed),
            Some(LiveKeyboardMapping {
                control: 1,
                explicit_other: 0,
                held: 0,
            })
        );
        assert!(receipt.live_mapping(&[]).is_none());
        assert!(
            receipt
                .live_mapping(&[observed[0].clone(), observed[0].clone()])
                .is_none()
        );
        assert_eq!(sys.calls()[..calls.len()], calls);
        assert_eq!(&sys.calls()[calls.len()..], &["keyboard-service-ids"]);

        let reset_dump = composite_keyboard_dump(&[(1, &[], &[]), (2, &native, &user_map)], true);
        sys.simulate(move |_, _| Some(Ran::ok(&reset_dump)));
        let reset = sys.keyboard_service_observations().unwrap();
        assert!(
            !receipt.matches(&reset[0]),
            "same identity, different native map"
        );
        assert!(receipt.live_mapping(&reset).is_none());
        let unknown_dump = composite_keyboard_dump(&[(1, &[], &[]), (2, &native, &user_map)], true)
            .replace("UserKeyMapping", "UnknownMapping");
        sys.simulate(move |_, _| Some(Ran::ok(&unknown_dump)));
        let unknown = sys.keyboard_service_observations().unwrap();
        assert!(unknown[0].unread().is_some());
        assert!(!receipt.matches(&unknown[0]));
        assert!(receipt.live_mapping(&unknown).is_none());

        // One physical/composite model can expose several HID services. Preserve an
        // explicit non-Control choice beside the genuinely verified Control service.
        let mut explicit = attachment(&who, 65534, 169);
        explicit.registries = vec![1];
        let get = explicit
            .inner
            .command()
            .args(["--get", "UserKeyMapping"])
            .display();
        let mixed_dump = composite_keyboard_dump(
            &[
                (1, &[(keyboard::CAPS_LOCK, option)], &[]),
                (2, &verified_native, &user_map),
            ],
            true,
        );
        let second_user = hid_user_row(2, &user_map);
        let users = format!(
            "{}{}",
            hid_user_row(1, &[]),
            second_user.strip_prefix("RegistryID Key Value\n").unwrap()
        );
        sys.simulate(move |_, cmd| {
            if cmd.display() == get {
                Some(Ran::ok(&users))
            } else if cmd.args.first().and_then(|arg| arg.to_str()) == Some("dump") {
                Some(Ran::ok(&mixed_dump))
            } else {
                None
            }
        });
        assert!(matches!(
            explicit.probe(&sys, &context),
            Probe::NotApplicable { .. }
        ));
        sys.keyboard_services = Some(Ok(vec![1, 2]));
        let current = sys.keyboard_service_observations().unwrap();
        let mut mixed = receipt;
        mixed.merge(explicit.verified().unwrap());
        assert_eq!(
            mixed.live_mapping(&current),
            Some(LiveKeyboardMapping {
                control: 1,
                explicit_other: 1,
                held: 0,
            })
        );
        mixed.merge(explicit.kept_receipt(&explicit.observed_services()));
        assert_eq!(
            mixed.live_mapping(&current),
            Some(LiveKeyboardMapping {
                control: 1,
                explicit_other: 0,
                held: 1,
            })
        );
        assert_eq!(sys.keyboard_changes().len(), 1, "passive reads never write");
    }

    #[test]
    fn automatic_attachments_preserve_explicit_saved_and_live_caps_choices() {
        let who = user("alice", 501);
        let option = 0x7_0000_00e2;
        let caps = keyboard::CAPS_LOCK;
        let as_value = |map: &[(i64, i64)]| {
            PValue::Array(
                map.iter()
                    .map(|(src, dst)| {
                        PValue::Dict(vec![
                            (SRC.into(), PValue::Int(*src)),
                            (DST.into(), PValue::Int(*dst)),
                        ])
                    })
                    .collect(),
            )
        };
        for (saved, native, user_map) in [
            (Some(as_value(&[])), vec![], vec![]),
            (Some(as_value(&[(caps, caps)])), vec![], vec![]),
            (Some(as_value(&[(caps, option)])), vec![], vec![]),
            (None, vec![(caps, caps)], vec![]),
            (None, vec![], vec![(caps, caps)]),
            (None, vec![(caps, option)], vec![]),
            (
                None,
                vec![(caps, option)],
                vec![(caps, keyboard::LEFT_CONTROL)],
            ),
        ] {
            let item = attachment(&who, 65534, 169);
            let mut sys = Fake::root();
            sys.euid = who.uid;
            let entries =
                saved.map_or_else(Vec::new, |value| vec![(item.inner.model.key(), value)]);
            let domain = crate::plist::document(&PValue::Dict(entries));
            sys.on(
                &Target::host("-g", &item.inner.model.key(), &who)
                    .export_cmd()
                    .display(),
                Ran::ok(&domain),
            );
            sys.on(
                &item
                    .inner
                    .command()
                    .args(["--get", "UserKeyMapping"])
                    .display(),
                Ran::ok(&hid_user_row(2, &user_map)),
            );
            sys.on(
                &hid_dump_command(&who).display(),
                Ran::ok(&composite_keyboard_dump(
                    &[(1, &[], &[]), (2, &native, &user_map)],
                    true,
                )),
            );
            let context = ctx(std::slice::from_ref(&who));
            assert!(matches!(
                item.probe(&sys, &context),
                Probe::NotApplicable { .. }
            ));
            assert!(
                item.observed_services().covers(&[2]),
                "an observed explicit choice resolves that service without changing it"
            );
            sys.logged_in(&who);
            sys.keyboard_services = Some(Ok(vec![2]));
            let receipt = item.verified().unwrap();
            let current = sys.keyboard_service_observations().unwrap();
            assert!(receipt.matches(&current[0]));
            assert_eq!(
                receipt.live_mapping(&current),
                Some(LiveKeyboardMapping {
                    control: 0,
                    explicit_other: 1,
                    held: 0,
                }),
                "a user Caps entry cannot bypass the earlier native output"
            );
            item.apply(&sys, &context).unwrap();
            assert!(
                item.verified().is_none(),
                "apply alone cannot publish a receipt"
            );
            assert!(sys.keyboard_changes().is_empty());
        }
    }

    #[test]
    fn attachment_coverage_discards_an_earlier_read_when_the_next_probe_sees_no_service() {
        let who = user("alice", 501);
        let item = attachment(&who, 65534, 169);
        let mut sys = Fake::root();
        sys.euid = who.uid;
        let get = item.inner.command().args(["--get", "UserKeyMapping"]);
        sys.on(&get.display(), Ran::ok(&hid_user_row(2, &[])));
        sys.on(&get.display(), Ran::ok("RegistryID Key Value\n"));
        sys.on(
            &hid_dump_command(&who).display(),
            Ran::ok(&hid_dump(&[(2, &[], &[])])),
        );
        sys.on(
            &Target::host("-g", &item.inner.model.key(), &who)
                .export_cmd()
                .display(),
            export("<dict/>"),
        );
        let context = ctx(std::slice::from_ref(&who));
        assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
        assert!(item.observed_services().covers(&[2]));
        assert!(matches!(
            item.probe(&sys, &context),
            Probe::NotApplicable { .. }
        ));
        assert!(!item.observed_services().covers(&[2]));
        assert!(sys.keyboard_changes().is_empty());
    }

    fn composite_keyboard_dump(maps: &[HidDumpRow<'_>], keyboard_usage: bool) -> String {
        let mut value = crate::plist::parse(hid_dump(maps).as_bytes()).unwrap();
        let PValue::Dict(fields) = &mut value else {
            unreachable!()
        };
        let (_, PValue::Array(services)) = &mut fields[0] else {
            unreachable!()
        };
        let PValue::Dict(service) = &mut services[1] else {
            unreachable!()
        };
        *service
            .iter_mut()
            .find(|(key, _)| key == "PrimaryUsage")
            .unwrap() = ("PrimaryUsage".into(), PValue::Int(2));
        let pair = |usage| {
            PValue::Dict(vec![
                ("DeviceUsagePage".into(), PValue::Int(1)),
                ("DeviceUsage".into(), PValue::Int(usage)),
            ])
        };
        let mut pairs = vec![pair(2)];
        if keyboard_usage {
            pairs.push(pair(6));
        }
        service.push(("DeviceUsagePairs".into(), PValue::Array(pairs)));
        crate::plist::document(&value)
    }

    #[test]
    fn a_composite_mouse_collection_carrying_keyboard_events_is_repaired_and_verified() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 65534,
                product: 169,
            },
            user: who.clone(),
            reading: LastReading::default(),
            expected: KeyboardCoverage::default(),
        };
        let mut sys = Fake::root();
        sys.euid = who.uid;
        let mapped = [(keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL)];
        let get = item.command().args(["--get", "UserKeyMapping"]).display();
        for _ in 0..4 {
            sys.on(&get, Ran::ok("RegistryID Key Value\n1 UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n2 UserKeyMapping (null)\n"));
        }
        sys.on(
            &get,
            Ran::ok("RegistryID Key Value\n2 UserKeyMapping (null)\n2 UserKeyMapping (null)\n"),
        );
        let dump = hid_dump_command(&who).display();
        for fixture in [
            composite_keyboard_dump(&[(1, &[], &mapped), (2, &[], &[])], true),
            composite_keyboard_dump(&[(1, &[], &mapped), (2, &[], &[])], true),
            composite_keyboard_dump(&[(1, &[], &mapped), (2, &mapped, &[])], true),
            composite_keyboard_dump(&[(1, &[], &mapped), (2, &mapped, &[])], false),
        ] {
            sys.on(&dump, Ran::ok(&fixture));
        }
        let context = ctx(&[who]);
        assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
        item.apply(&sys, &context).unwrap();
        let changes = sys.keyboard_changes();
        assert_eq!(
            changes.len(),
            1,
            "the already-correct primary interface is preserved"
        );
        assert_eq!(changes[0].registry, 2);
        assert_eq!(changes[0].native, Some(mapped.to_vec()));
        assert_eq!(changes[0].user, None);
        assert!(matches!(item.probe(&sys, &context), Probe::Ok { .. }));
        assert!(
            matches!(item.probe(&sys, &context), Probe::Unusable { .. }),
            "a true mouse cannot become a keyboard write target"
        );
        assert!(
            matches!(item.probe(&sys, &context), Probe::Unusable { .. }),
            "an OR matcher returning a service twice cannot trigger duplicate writes"
        );
        assert_eq!(sys.keyboard_changes().len(), 1);
    }

    #[test]
    fn effective_caps_uses_native_then_user_mapping_and_activation_keeps_other_keys() {
        let caps = keyboard::CAPS_LOCK;
        let control = keyboard::LEFT_CONTROL;
        let option = 0x7_0000_00E2;
        let native = HidMapping {
            registry: 1,
            prefix: KeyboardPrefix::default(),
            native: vec![(caps, control)],
            user: Vec::new(),
        };
        assert_eq!(native.caps_lock(), control);
        assert_eq!(
            merged_mapping(&[native]).unwrap(),
            None,
            "native Control needs no user mapping"
        );

        for (native, user) in [
            (
                vec![(caps, option), (option, keyboard::RIGHT_CONTROL)],
                vec![(caps, control)],
            ),
            (
                vec![(caps, control), (option, keyboard::RIGHT_CONTROL)],
                vec![(caps, control), (control, option)],
            ),
        ] {
            let before = HidMapping {
                registry: 1,
                prefix: KeyboardPrefix::default(),
                native,
                user,
            };
            let other_keys = [control, keyboard::RIGHT_CONTROL, option, 0x7_0000_00E3];
            let other_outputs = other_keys.map(|key| before.output(key));
            assert_eq!(
                before.caps_lock(),
                option,
                "a user Caps entry cannot bypass the earlier native lookup"
            );
            let mut change = merged_mapping(std::slice::from_ref(&before))
                .unwrap()
                .unwrap();
            let change = change.remove(0);
            assert_eq!(change.user, None);
            assert_eq!(change.before_user, before.user);
            let after = after_change(before, change);
            assert!(matches!(
                after.caps_lock(),
                keyboard::LEFT_CONTROL | keyboard::RIGHT_CONTROL
            ));
            assert!(after.native.contains(&(option, keyboard::RIGHT_CONTROL)));
            assert_eq!(other_keys.map(|key| after.output(key)), other_outputs);
            assert_eq!(merged_mapping(&[after]).unwrap(), None);
        }
    }

    #[test]
    fn activation_preserves_other_physical_keys_that_enter_the_user_caps_mapping() {
        let caps = keyboard::CAPS_LOCK;
        let left = keyboard::LEFT_CONTROL;
        let right = keyboard::RIGHT_CONTROL;
        let option = 0x7_0000_00E2;
        let command = 0x7_0000_00E3;
        let before = HidMapping {
            registry: 1,
            prefix: KeyboardPrefix::default(),
            native: vec![(caps, option), (command, caps)],
            user: vec![(left, option)],
        };
        let command_before = before.output(command);
        let mut changes = merged_mapping(std::slice::from_ref(&before))
            .unwrap()
            .unwrap();
        let change = changes.remove(0);
        assert_eq!(
            change.user, None,
            "adding user Caps would also change physical Command"
        );
        let after = after_change(before, change);
        assert_eq!(after.caps_lock(), right);
        assert_eq!(after.output(command), command_before);
        assert!(after.native.contains(&(command, caps)));
        assert!(!after.user.iter().any(|(source, _)| *source == caps));

        let blocked = HidMapping {
            registry: 2,
            prefix: KeyboardPrefix::default(),
            native: vec![(caps, option), (command, caps)],
            user: vec![(left, option), (right, option)],
        };
        assert!(
            merged_mapping(&[blocked]).is_err(),
            "no safe Control preimage can be invented"
        );

        let safe = HidMapping {
            registry: 3,
            prefix: KeyboardPrefix::default(),
            native: vec![(caps, option), (command, option)],
            user: vec![(left, option), (right, option)],
        };
        let command_before = safe.output(command);
        let mut changes = merged_mapping(std::slice::from_ref(&safe))
            .unwrap()
            .unwrap();
        let after = after_change(safe, changes.remove(0));
        assert_eq!(after.caps_lock(), left);
        assert_eq!(after.output(command), command_before);
        assert!(after.native.contains(&(command, option)));
        assert_eq!(
            after.user.iter().find(|(source, _)| *source == left),
            Some(&(left, option))
        );
    }

    #[test]
    fn filter_state_is_bound_by_service_identity_and_never_guessed_from_row_order() {
        let caps = keyboard::CAPS_LOCK;
        let control = keyboard::LEFT_CONTROL;
        let user = vec![(caps, control)];
        let matched = vec![(1, user.clone()), (2, user.clone())];
        let native = vec![(caps, 0x7_0000_00E2)];
        let dump = hid_dump(&[(2, &native, &user), (1, &[], &user)]);
        let observed = live_hid_mappings(&dump, &matched).unwrap();
        assert_eq!(observed[0].caps_lock(), control);
        assert_eq!(observed[1].caps_lock(), 0x7_0000_00E2);
        for incomplete in [
            hid_dump(&[(1, &[], &user)]),
            hid_dump(&[(1, &[], &user), (1, &[], &user), (2, &native, &user)]),
            hid_dump(&[(1, &[], &user), (2, &native, &[])]),
            dump.replace("IOHIDKeyboardFilter", "UnknownFilter"),
            dump.replace("HIDKeyboardModifierMappingPairs", "UnknownMapping"),
            dump.replace("<key>Dst</key>", "<key>Unknown</key>"),
        ] {
            assert!(live_hid_mappings(&incomplete, &matched).is_err());
        }
        assert!(parse_hid_mappings("1 UserKeyMapping (null)\n1 UserKeyMapping (null)").is_err());
        assert!(parse_hid_mappings("garbage\n1 UserKeyMapping (null)").is_err());
    }

    #[test]
    fn immediate_mapping_sets_only_the_matched_keyboard_and_verifies_the_real_hid_output() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 1452,
                product: 598,
            },
            user: who.clone(),
            reading: LastReading::default(),
            expected: KeyboardCoverage::default(),
        };
        let mut sys = Fake::root();
        sys.euid = who.uid;
        let get = item.command().args(["--get", "UserKeyMapping"]).display();
        sys.on(&get, Ran::ok("RegistryID Key Value\n10002160e UserKeyMapping (null)\n100021604 UserKeyMapping (null)\n"));
        let dump = hid_dump_command(&who).display();
        sys.simulate(move |sys, cmd| {
            if cmd.display() != dump {
                return None;
            }
            let changes = sys.keyboard_changes();
            let rows: Vec<_> = if changes.is_empty() {
                vec![
                    (0x10002160e, &[][..], &[][..]),
                    (0x100021604, &[][..], &[][..]),
                ]
            } else {
                changes
                    .iter()
                    .rev()
                    .map(|change| {
                        (
                            change.registry,
                            change.native.as_deref().unwrap_or(&change.before_native),
                            change.user.as_deref().unwrap_or(&change.before_user),
                        )
                    })
                    .collect()
            };
            Some(Ran::ok(&hid_dump(&rows)))
        });
        let c = ctx(&[who]);
        assert!(matches!(item.probe(&sys, &c), Probe::Change { .. }));
        // No service maps Caps Lock yet: System State reads the key as Caps Lock.
        assert_eq!(item.detail(&sys, &c), Some(Reading::CapsLock.token()));
        item.apply(&sys, &c).unwrap();
        let changes = sys.keyboard_changes();
        assert_eq!(
            changes
                .iter()
                .map(|change| change.registry)
                .collect::<Vec<_>>(),
            [0x10002160e, 0x100021604]
        );
        assert!(changes.iter().all(|change| change.user.is_none()));
        assert!(matches!(item.probe(&sys, &c), Probe::Ok { .. }));
        assert_eq!(item.detail(&sys, &c), None);
    }

    #[test]
    fn a_successful_set_cannot_verify_caps_while_the_native_table_still_redirects_it() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 1452,
                product: 598,
            },
            user: who.clone(),
            reading: LastReading::default(),
            expected: KeyboardCoverage::default(),
        };
        let mut sys = Fake::root();
        sys.euid = who.uid;
        let mapped = [(keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL)];
        let native = [(keyboard::CAPS_LOCK, 0x7_0000_00E2)];
        sys.on(&item.command().args(["--get", "UserKeyMapping"]).display(), Ran::ok("RegistryID Key Value\n100021604 UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n"));
        sys.on(
            &hid_dump_command(&who).display(),
            Ran::ok(&hid_dump(&[(0x100021604, &native, &mapped)])),
        );
        let rows = crate::engine::run(
            &[Box::new(item)],
            crate::model::Mode::Apply,
            &sys,
            &ctx(&[who]),
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, crate::model::Outcome::Failed);
        assert_eq!(sys.keyboard_changes().len(), 1);
        assert_eq!(sys.keyboard_changes()[0].registry, 0x100021604);
    }

    #[test]
    fn upstream_caps_remaps_are_never_reported_ready_or_rewritten_as_native_caps() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 1452,
                product: 598,
            },
            user: who.clone(),
            reading: LastReading::default(),
            expected: KeyboardCoverage::default(),
        };
        let mut sys = Fake::root();
        sys.euid = who.uid;
        sys.on(
            &item.command().args(["--get", "UserKeyMapping"]).display(),
            Ran::ok("RegistryID Key Value\n100021604 UserKeyMapping (null)\n"),
        );
        let native = [(keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL)];
        let mut prefix = crate::port::keyboard_prefix::tests::fields();
        let fn_map = &mut prefix
            .iter_mut()
            .find(|(name, _)| name == "FnFunctionUsageMap")
            .unwrap()
            .1;
        *fn_map =
            crate::port::keyboard_prefix::tests::pairs(&[(keyboard::CAPS_LOCK, 0x7_0000_00e2)]);
        // With FnKeyMode=0 and no Fn held, Apple applies Caps→Option before the native
        // table. A native Caps→Control entry therefore does not establish physical Control.
        sys.on(
            &hid_dump_command(&who).display(),
            Ran::ok(&hid_dump_with_prefix(
                &[(0x100021604, &native, &[])],
                &prefix,
            )),
        );
        let context = ctx(std::slice::from_ref(&who));
        assert!(matches!(item.probe(&sys, &context), Probe::Unusable { .. }));
        assert!(item.apply(&sys, &context).is_err());
        assert!(sys.keyboard_changes().is_empty());

        *prefix
            .iter_mut()
            .find(|(name, _)| name == "FnFunctionUsageMap")
            .unwrap() = (
            "FnFunctionUsageMap".into(),
            crate::port::keyboard_prefix::tests::pairs(&[
                (0x7_0000_003a, 0xc_0000_006f),
                (keyboard::CAPS_LOCK, keyboard::CAPS_LOCK),
            ]),
        );
        let mut ordinary = Fake::root();
        ordinary.euid = who.uid;
        ordinary.on(
            &item.command().args(["--get", "UserKeyMapping"]).display(),
            Ran::ok("RegistryID Key Value\n100021604 UserKeyMapping (null)\n"),
        );
        ordinary.on(
            &hid_dump_command(&who).display(),
            Ran::ok(&hid_dump_with_prefix(
                &[(0x100021604, &native, &[])],
                &prefix,
            )),
        );
        assert!(
            matches!(item.probe(&ordinary, &context), Probe::Ok { .. }),
            "ordinary function maps and identity Caps remain usable"
        );

        let historical = crate::port::keyboard_prefix::tests::historical_fields();
        let old_dump = hid_dump_with_prefix(&[(0x100021604, &[], &[])], &historical);
        let old_maps = live_hid_mappings(&old_dump, &[(0x100021604, Vec::new())]).unwrap();
        let mut changes = merged_mapping(&old_maps).unwrap().unwrap();
        assert_eq!(
            changes.len(),
            1,
            "complete historical state still permits setup"
        );
        assert_eq!(changes[0].before_prefix, old_maps[0].prefix);
        assert_eq!(
            after_change(old_maps.into_iter().next().unwrap(), changes.remove(0)).caps_lock(),
            keyboard::LEFT_CONTROL
        );
    }

    #[test]
    fn activation_for_another_user_never_reaches_the_native_writer() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 1452,
                product: 598,
            },
            user: who.clone(),
            reading: LastReading::default(),
            expected: KeyboardCoverage::default(),
        };
        for actor in [0, 502] {
            let mut sys = Fake::root();
            sys.euid = actor;
            assert!(item.apply(&sys, &ctx(std::slice::from_ref(&who))).is_err());
            assert!(sys.keyboard_changes().is_empty());
            assert!(sys.calls().is_empty());
        }
    }

    #[test]
    fn an_incomplete_hid_record_never_overwrites_an_omitted_services_remaps() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 1452,
                product: 598,
            },
            user: who.clone(),
            reading: LastReading::default(),
            expected: KeyboardCoverage::default(),
        };
        let sys = Fake::root();
        let get = item.command().args(["--get", "UserKeyMapping"]).display();
        sys.on(
            &get,
            Ran::ok("RegistryID Key Value\n100021604 UserKeyMapping (null)\n"),
        );
        sys.refuse_complete_stdout(&get, "stdout exceeds complete capture bound");
        let rows = crate::engine::run(
            &[Box::new(item)],
            crate::model::Mode::Apply,
            &sys,
            &ctx(&[who]),
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, crate::model::Outcome::Unknown);
        assert!(sys.calls().iter().all(|call| !call.contains("--set")));
        assert!(sys.keyboard_changes().is_empty());
    }

    #[test]
    fn dock_metadata_is_ignored_but_pin_order_extra_pins_and_stacks_are_not() {
        let want = dock_apps(&usual_dock_apps());
        let PValue::Array(mut pins) = want.clone() else {
            unreachable!()
        };
        let PValue::Dict(first) = &mut pins[0] else {
            unreachable!()
        };
        first.push(("GUID".into(), PValue::Int(123)));
        assert!(same_apps(
            &Fake::root(),
            &PValue::Array(pins.clone()),
            &want
        ));
        pins.swap(0, 1);
        assert!(!same_apps(
            &Fake::root(),
            &PValue::Array(pins.clone()),
            &want
        ));
        pins.push(PValue::Dict(vec![]));
        assert!(!same_apps(&Fake::root(), &PValue::Array(pins), &want));
    }

    #[test]
    fn dock_bundle_labels_cannot_hide_missing_or_different_file_targets() {
        let want = dock_apps(&usual_dock_apps());
        let xml = crate::plist::fragment(&want);
        for wrong in [
            "file:///Applications/Missing.app/",
            "file://another-host/Applications/aterm.app/",
            "file:///Applications/aterm.app/?different-target",
            "file:///Applications/aterm.app/#different-target",
            "file:///Applications/%QQaterm.app/",
            "file:///Applications/%00aterm.app/",
        ] {
            let changed = xml.replace("file:///Applications/aterm.app/", wrong);
            let changed = crate::plist::parse(changed.as_bytes()).unwrap();
            assert!(!same_apps(&Fake::root(), &changed, &want), "{wrong}");
        }
        let missing = xml.replacen("<key>file-data</key>", "<key>unusable-file-data</key>", 1);
        assert!(!same_apps(
            &Fake::root(),
            &crate::plist::parse(missing.as_bytes()).unwrap(),
            &want
        ));
        let normalized = xml
            .replace(
                "file:///Applications/aterm.app/",
                "file://localhost/Applications/%61term.app",
            )
            .replace(
                "file:///Applications/Safari.app/",
                "file:///Applications/Safari.app",
            );
        assert!(same_apps(
            &Fake::root(),
            &crate::plist::parse(normalized.as_bytes()).unwrap(),
            &want
        ));
    }

    #[test]
    fn missing_or_wrong_installed_dock_apps_cannot_be_reported_configured() {
        let who = user("alice", 501);
        let safari = DOCK_APPS[2].0;
        for (identity, expected) in [
            (None, format!("Safari is not installed at {safari}")),
            (
                Some("com.example.another-app"),
                format!("{safari} is not the requested Safari application"),
            ),
        ] {
            let (sys, desk) = desktop_with(&who, Desk::before(), false);
            for (path, bundle, _) in &DOCK_APPS[..2] {
                installed_at(&sys, &who, path, bundle);
            }
            if let Some(bundle) = identity {
                installed_at(&sys, &who, safari, bundle);
            }
            let run = pass(
                &sys,
                selected_desktop_items(&who, &[Setting::Dock]),
                Mode::Apply,
            );
            assert_eq!(run.rows[0].outcome, Outcome::Unknown, "{:?}", run.rows);
            assert_eq!(
                run.rows[0].why.as_deref(),
                Some(format!("{expected}; Dock left untouched").as_str())
            );
            let desk = desk.lock().unwrap();
            assert!(
                !desk
                    .events
                    .iter()
                    .any(|e| e.starts_with("write ") || e.starts_with("kill ")),
                "{:?}",
                desk.events
            );
        }
    }

    #[test]
    fn the_dock_pins_the_aterm_in_the_accounts_own_applications_folder() {
        let who = user("alice", 501);
        let usual_path = DOCK_APPS[0].0;
        let own = "/Users//alice/Applications/aterm.app";
        let pins_at = |aterm: &str| {
            let mut apps = vec![(aterm.to_owned(), DOCK_APPS[0].1, DOCK_APPS[0].2)];
            apps.extend(
                DOCK_APPS[1..]
                    .iter()
                    .map(|&(path, bundle, label)| (path.to_owned(), bundle, label)),
            );
            dock_apps(&apps)
        };
        let aterm = Some("com.aterm.aterm");
        for (usual, home, running, pinned) in [
            (None, aterm, None, Ok(own)),
            (Some("com.example.another-app"), aterm, None, Ok(own)),
            (aterm, aterm, None, Ok(usual_path)),
            (aterm, aterm, Some(own), Ok(own)),
            (aterm, aterm, Some(usual_path), Ok(usual_path)),
            (aterm, None, Some(own), Ok(usual_path)),
            (
                None,
                None,
                None,
                Err(format!("aterm is not installed at {usual_path} or {own}")),
            ),
            (
                Some("com.example.another-app"),
                None,
                None,
                Err(format!(
                    "no aterm to pin ({usual_path} is not the requested aterm application, \
                     nothing at {own})"
                )),
            ),
            (
                None,
                Some("com.aterm.aterm.dev"),
                Some(own),
                Err(format!(
                    "no aterm to pin ({own} is not the requested aterm application, nothing at \
                     {usual_path})"
                )),
            ),
        ] {
            let (mut sys, desk) = desktop_with(&who, Desk::before(), false);
            sys.running_app = running.map(PathBuf::from);
            for (path, bundle, _) in &DOCK_APPS[1..] {
                installed_at(&sys, &who, path, bundle);
            }
            if let Some(bundle) = usual {
                installed_at(&sys, &who, usual_path, bundle);
            }
            if let Some(bundle) = home {
                installed_at(&sys, &who, own, bundle);
            }
            let run = pass(
                &sys,
                selected_desktop_items(&who, &[Setting::Dock]),
                Mode::Apply,
            );
            let case = format!("{usual:?} {home:?} {running:?}");
            match pinned {
                Ok(aterm) => {
                    assert_eq!(
                        run.rows[0].outcome,
                        Outcome::Changed,
                        "{case}: {:?}",
                        run.rows
                    );
                    let desk = desk.lock().unwrap();
                    let now = desk.read(DOCK, "persistent-apps").unwrap();
                    assert!(same_apps(&sys, now, &pins_at(aterm)), "{case}: {now:?}");
                }
                Err(expected) => {
                    assert_eq!(
                        run.rows[0].outcome,
                        Outcome::Unknown,
                        "{case}: {:?}",
                        run.rows
                    );
                    let why = run.rows[0].why.clone().unwrap_or_default();
                    assert_eq!(why, format!("{expected}; Dock left untouched"), "{case}");
                    assert!(
                        !desk
                            .lock()
                            .unwrap()
                            .events
                            .iter()
                            .any(|e| e.starts_with("write ")),
                        "{case}"
                    );
                }
            }
        }
    }

    /// The pin is the copy this aterm runs from wherever that copy lasts, and never a copy
    /// that goes away (an App Translocation copy, a disk image): with nothing installed where
    /// aterm installs, the row says it runs from such a copy, and the Dock is left untouched.
    /// Negative control: a pin limited to `/Applications` and `~/Applications` reads this
    /// lasting copy as not installed.
    #[test]
    fn the_dock_pins_the_running_copy_where_it_lasts_and_never_a_copy_that_goes_away() {
        let who = user("alice", 501);
        let usual_path = DOCK_APPS[0].0;
        let own = "/Users//alice/Applications/aterm.app";
        let lasting = "/Users//alice/Developer/aterm.app";
        let translocated = "/private/var/folders/x/AppTranslocation/Y/d/aterm.app";
        for (running, usual, pinned) in [
            (lasting, false, Ok(lasting)),
            (translocated, true, Ok(usual_path)),
            (
                translocated,
                false,
                Err(format!(
                    "aterm is not installed at {usual_path} or {own}; it runs from \
                     {translocated}, a copy that goes away (a disk image or App Translocation \
                     copy): move aterm.app to /Applications; Dock left untouched"
                )),
            ),
        ] {
            let (mut sys, desk) = desktop_with(&who, Desk::before(), false);
            sys.running_app = Some(PathBuf::from(running));
            for (path, bundle, _) in &DOCK_APPS[1..] {
                installed_at(&sys, &who, path, bundle);
            }
            installed_at(&sys, &who, running, "com.aterm.aterm");
            if usual {
                installed_at(&sys, &who, usual_path, "com.aterm.aterm");
            }
            let run = pass(
                &sys,
                selected_desktop_items(&who, &[Setting::Dock]),
                Mode::Apply,
            );
            let case = format!("{running} {usual}");
            match pinned {
                Ok(aterm) => {
                    assert_eq!(
                        run.rows[0].outcome,
                        Outcome::Changed,
                        "{case}: {:?}",
                        run.rows
                    );
                    let mut apps = vec![(aterm.to_owned(), DOCK_APPS[0].1, DOCK_APPS[0].2)];
                    apps.extend(
                        DOCK_APPS[1..]
                            .iter()
                            .map(|&(path, bundle, label)| (path.to_owned(), bundle, label)),
                    );
                    let desk = desk.lock().unwrap();
                    let now = desk.read(DOCK, "persistent-apps").unwrap();
                    assert!(same_apps(&sys, now, &dock_apps(&apps)), "{case}: {now:?}");
                }
                Err(expected) => {
                    assert_eq!(
                        run.rows[0].outcome,
                        Outcome::Unknown,
                        "{case}: {:?}",
                        run.rows
                    );
                    assert_eq!(
                        run.rows[0].why.as_deref(),
                        Some(expected.as_str()),
                        "{case}"
                    );
                    assert!(
                        !desk
                            .lock()
                            .unwrap()
                            .events
                            .iter()
                            .any(|e| e.starts_with("write ")),
                        "{case}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_pin_reads_back_as_the_path_it_was_made_from() {
        for path in [
            "/Applications/aterm.app",
            "/System/Applications/System Settings.app",
            "/Volumes/Work #2/bob/Applications/aterm.app",
            "/Users//what?/Applications/aterm.app",
            "/Volumes/100%/bob/Applications/aterm.app",
            "/Volumes/50%20off/bob/Applications/aterm.app",
            "/Users//zo\u{eb} (old)+new/Applications/aterm.app",
        ] {
            let PValue::Array(pins) = dock_apps(&[(path.to_owned(), "com.aterm.aterm", "aterm")])
            else {
                unreachable!()
            };
            assert_eq!(dock_app_path(&pins[0]), Some(PathBuf::from(path)), "{path}");
        }
        assert_eq!(
            file_url("/System/Applications/System Settings.app"),
            "file:///System/Applications/System%20Settings.app/",
            "the URL the Dock itself writes for a space"
        );
    }

    #[test]
    fn an_applications_folder_reached_through_a_link_still_pins_the_running_copy() {
        let who = user("alice", 501);
        let own = "/Users//alice/Applications/aterm.app";
        let real = "/Volumes/Apps/alice/aterm.app";
        let pins_at = |aterm: &str| {
            let mut apps = usual_dock_apps();
            apps[0].0 = aterm.to_owned();
            dock_apps(&apps)
        };
        let (mut sys, desk) = desktop_of(&who, Desk::before());
        installed_at(&sys, &who, own, DOCK_APPS[0].1);
        sys.link("/Users//alice/Applications", "/Volumes/Apps/alice");
        sys.running_app = Some(PathBuf::from(real));
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(run.rows[0].outcome, Outcome::Changed, "{:?}", run.rows);
        let desk = desk.lock().unwrap();
        let now = desk.read(DOCK, "persistent-apps").unwrap();
        assert!(same_apps(&Fake::root(), now, &pins_at(own)), "{now:?}");
        assert!(same_apps(&sys, &pins_at(real), &pins_at(own)));
        assert!(!same_apps(&sys, &pins_at(real), &pins_at(DOCK_APPS[0].0)));
    }

    #[test]
    fn dock_plan_is_read_only_and_covers_every_persistent_static_and_recent_section() {
        let who = user("alice", 501);
        let item = desktop(&who, DesktopKind::Dock);
        let sys = Fake::root();
        installed_dock_apps(&sys, &who);
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.dock -",
            export("<dict/>"),
        );
        let changes = item.changes(&sys).unwrap();
        let keys: Vec<_> = changes
            .iter()
            .map(|(target, _, _)| target.key.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "autohide",
                "persistent-apps",
                "persistent-others",
                "static-apps",
                "static-others",
                "recent-apps",
                "show-recents",
                "static-only",
                "minimize-to-application"
            ]
        );
        assert!(sys.calls().iter().all(|call| call.contains(" export ")
            || call.contains("/usr/bin/plutil -extract CFBundleIdentifier")));
        let mut c = ctx(&[who]);
        c.disrupt = false;
        assert!(matches!(item.probe(&sys, &c), Probe::Deferred { .. }));
        let undo = item.revert().unwrap();
        assert_eq!(undo.lines().count(), keys.len());
        assert!(
            undo.lines()
                .all(|line| line.contains("/usr/bin/defaults delete") && line.contains("  # "))
        );
    }

    #[test]
    fn a_desktop_reading_names_only_values_it_read_and_three_other_apps_are_not_the_three() {
        let who = user("alice", 501);
        let target = |domain: &str, key: &str| Target::user(domain, key, &who);
        let pin = || PValue::Dict(Vec::new());
        let dock = |pins: usize| -> Changes {
            vec![(
                target("com.apple.dock", "persistent-apps"),
                Read::Value(PValue::Array(vec![pin(); pins])),
                dock_apps(&usual_dock_apps()),
            )]
        };
        let PValue::Array(wanted) = dock_apps(&usual_dock_apps()) else {
            panic!("the Dock's pins are a list");
        };
        assert_eq!(
            DesktopKind::Dock.reading(&dock(wanted.len())),
            Some(Reading::Apps {
                pinned: wanted.len(),
                wanted: wanted.len()
            })
        );
        assert_eq!(
            Reading::Apps {
                pinned: wanted.len(),
                wanted: wanted.len()
            }
            .word()
            .as_deref(),
            Some("Other apps")
        );
        let hidden: Changes = vec![(
            target("com.apple.finder", "CreateDesktop"),
            Read::Value(PValue::Bool(false)),
            PValue::Bool(true),
        )];
        assert_eq!(
            DesktopKind::Desktop.reading(&hidden),
            Some(Reading::IconsHidden)
        );
        // An absent key holds macOS's default, which this does not read.
        let absent: Changes = vec![(
            target("com.apple.finder", "CreateDesktop"),
            Read::Absent,
            PValue::Bool(true),
        )];
        assert_eq!(DesktopKind::Desktop.reading(&absent), None);
        assert_eq!(DesktopKind::Dock.reading(&absent), None);
    }

    /// The marker's barriers ([`crate::init_activation::begin`]) come before any write: a pass
    /// whose barrier fails writes nothing, and its retry, which finds the marker the failed pass
    /// left, orders the whole chain again before its first write. A first pass orders only the
    /// directories whose entries it changed (the marker's, and the parent of each it created),
    /// and a later marker in the directory that is there orders that directory alone.
    #[test]
    fn a_failed_namespace_flush_allows_no_write_and_a_retry_flushes_the_whole_chain() {
        let who = user("alice", 501);
        let windowmanager = "alice$ /usr/bin/defaults export com.apple.WindowManager -";
        let finder = "alice$ /usr/bin/defaults export com.apple.finder -";
        let c = ctx(std::slice::from_ref(&who));
        let chain = [
            "/Users//alice/Library/Application Support/aterm/init-activation",
            "/Users//alice/Library/Application Support/aterm",
            "/Users//alice/Library/Application Support",
            "/Users//alice/Library",
            "/Users//alice",
            "/Users",
            "/",
        ];
        let ordered = |calls: &[String]| -> Vec<String> {
            calls
                .iter()
                .filter(|call| call.starts_with("ordered-dir "))
                .cloned()
                .collect()
        };
        let expected: Vec<String> = chain
            .iter()
            .map(|dir| format!("ordered-dir {dir}"))
            .collect();
        // The first pass creates `aterm` and `init-activation`: it changes the entries of
        // those two and of `Application Support`.
        for (failed_at, directory) in chain[..3].iter().enumerate() {
            let mut sys = Fake::root();
            sys.euid = who.uid;
            for home in &chain[2..5] {
                sys.stat_of(
                    home,
                    Stat {
                        uid: who.uid,
                        gid: who.gid,
                        mode: 0o700,
                        dir: true,
                        symlink: false,
                        len: 0,
                    },
                );
            }
            sys.on(windowmanager, export("<dict><key>EnableStandardClickToShowDesktop</key><false/><key>StandardHideDesktopIcons</key><true/><key>StageManagerHideDesktopIcons</key><true/></dict>"));
            sys.on(
                finder,
                export("<dict><key>CreateDesktop</key><true/></dict>"),
            );
            for key in ["StandardHideDesktopIcons", "StageManagerHideDesktopIcons"] {
                let cmd = Target::user("com.apple.WindowManager", key, &who)
                    .write_cmd(&PValue::Bool(false));
                sys.on(&cmd.display(), Ran::ok(""));
            }
            sys.durable_dir_fails(Path::new(directory), Some(std::io::ErrorKind::Other));
            let item = desktop(&who, DesktopKind::Desktop);
            assert!(item.apply(&sys, &c).is_err());
            assert!(crate::init_activation::pending(&sys, &who, "desktop").unwrap());
            let calls = sys.calls();
            assert!(
                calls
                    .iter()
                    .all(|call| !call.contains("/usr/bin/defaults write")
                        && !call.contains("pgrep")
                        && !call.contains("killall"))
            );
            assert_eq!(ordered(&calls), expected[..=failed_at]);
            sys.durable_dir_fails(Path::new(directory), None);
            let retry = calls.len();
            item.apply(&sys, &c).unwrap();
            assert!(crate::init_activation::pending(&sys, &who, "desktop").unwrap());
            let calls = sys.calls();
            let retry_calls = &calls[retry..];
            let first_write = retry_calls
                .iter()
                .position(|call| call.contains("/usr/bin/defaults write"))
                .unwrap();
            assert_eq!(ordered(&retry_calls[..first_write]), expected);
            assert_eq!(
                retry_calls
                    .iter()
                    .filter(|call| call.contains("/usr/bin/defaults write"))
                    .count(),
                2
            );
            // Another family's first marker in the directory that is there.
            let later = sys.calls().len();
            crate::init_activation::begin(&sys, &who, "dock").unwrap();
            assert_eq!(ordered(&sys.calls()[later..]), expected[..1]);
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.starts_with("durable-dir ")),
                "a barrier, never a full flush: {:?}",
                sys.calls()
            );
        }
    }

    const TILES: [&str; 3] = ["persistent-apps", "persistent-others", "recent-apps"];

    type Loaded = Vec<(&'static str, PValue)>;

    type Starting = Option<(Loaded, u32)>;

    struct Desk {
        prefs: Vec<(String, String, PValue)>,
        /// Each running process, its pid, and, while it is starting, the tiles it read and
        /// the sleeps ([`System::sleep`]) left until its start is past.
        running: Vec<(&'static str, u32, Starting)>,
        next_pid: u32,
        /// How many sleeps a Dock takes to get past its start.
        start: u32,
        /// The pins a managed Dock shows once it is past its start, whatever was written.
        managed: Option<PValue>,
        /// A process launchd does not start again once it is killed.
        stays_down: Option<&'static str>,
        /// A process whose `killall` fails (exit 2) and signals nothing.
        unkillable: Option<&'static str>,
        /// A `defaults write` of this domain and key fails, writing nothing.
        unwritable: Option<(&'static str, &'static str)>,
        /// How many `launchctl print` session checks fail before one answers.
        session_failures: u32,
        /// A domain macOS keeps from the reader, as it keeps another app's container from a
        /// process without Full Disk Access: its export prints an empty dictionary
        /// (measured); a write is assumed to exit 0 and never be read back (not measured).
        denied: Option<&'static str>,
        /// What happened, in order: `write <domain> <key>`, `kill <process>`, `<process>
        /// <pid> wrote back what it read at its start`, `Dock <pid> past its start`.
        events: Vec<String>,
    }

    fn pin(label: &str) -> PValue {
        PValue::Dict(vec![(
            "tile-data".into(),
            PValue::Dict(vec![("file-label".into(), PValue::Str(label.into()))]),
        )])
    }

    impl Desk {
        fn before() -> Self {
            Self {
                prefs: vec![
                    (
                        DOCK.into(),
                        "persistent-apps".into(),
                        PValue::Array(vec![pin("Mail"), pin("Music")]),
                    ),
                    (
                        DOCK.into(),
                        "persistent-others".into(),
                        PValue::Array(vec![pin("Downloads")]),
                    ),
                    (
                        DOCK.into(),
                        "recent-apps".into(),
                        PValue::Array(vec![pin("Notes")]),
                    ),
                    (DOCK.into(), "show-recents".into(), PValue::Bool(true)),
                    (
                        WINDOW_MANAGER.into(),
                        "StandardHideDesktopIcons".into(),
                        PValue::Bool(true),
                    ),
                    (
                        NOTIFICATION_CENTER.into(),
                        "widgets".into(),
                        PValue::Dict(vec![
                            ("vers".into(), PValue::Int(1)),
                            (
                                "instances".into(),
                                PValue::Array(vec![PValue::Data(vec![42])]),
                            ),
                        ]),
                    ),
                ],
                running: vec![
                    ("Dock", 100, None),
                    ("Finder", 200, None),
                    ("NotificationCenter", 300, None),
                ],
                next_pid: 1000,
                start: 1,
                managed: None,
                stays_down: None,
                unkillable: None,
                unwritable: None,
                session_failures: 0,
                denied: None,
                events: Vec::new(),
            }
        }

        fn read(&self, domain: &str, key: &str) -> Option<&PValue> {
            self.prefs
                .iter()
                .find(|(d, k, _)| d == domain && k == key)
                .map(|(_, _, value)| value)
        }

        fn write(&mut self, domain: &str, key: &str, value: PValue) {
            self.prefs.retain(|(d, k, _)| !(d == domain && k == key));
            self.prefs.push((domain.into(), key.into(), value));
        }

        fn count(&self, event: &str) -> usize {
            self.events.iter().filter(|e| *e == event).count()
        }

        fn starting(&self) -> Vec<(&'static str, u32)> {
            self.running
                .iter()
                .filter(|(_, _, starting)| starting.is_some())
                .map(|(name, pid, _)| (*name, *pid))
                .collect()
        }

        fn answer(&mut self, fake: &Fake, cmd: &Cmd) -> Option<Ran> {
            let args: Vec<String> = cmd
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match (cmd.program.to_str()?, args.as_slice()) {
                ("/usr/bin/defaults", ["export", domain, "-"]) if self.denied == Some(*domain) => {
                    Some(Ran::ok(&crate::plist::document(&PValue::Dict(Vec::new()))))
                }
                ("/usr/bin/defaults", ["export", domain, "-"]) => {
                    let entries = self
                        .prefs
                        .iter()
                        .filter(|(d, _, _)| d == domain)
                        .map(|(_, key, value)| (key.clone(), value.clone()))
                        .collect();
                    Some(Ran::ok(&crate::plist::document(&PValue::Dict(entries))))
                }
                ("/usr/bin/defaults", ["write", domain, key, value @ ..]) => {
                    let value = match value {
                        ["-bool", b] => PValue::Bool(*b == "true"),
                        ["-int", i] => PValue::Int(i.parse().ok()?),
                        ["-string", text] => PValue::Str((*text).into()),
                        [fragment] => crate::plist::parse(fragment.as_bytes()).ok()?,
                        _ => return None,
                    };
                    if self.unwritable == Some((domain, key)) {
                        return Some(Ran::exit(1, "injected write failure"));
                    }
                    if self.denied == Some(*domain) {
                        self.events.push(format!("write {domain} {key}"));
                        return Some(Ran::ok(""));
                    }
                    self.events.push(format!("write {domain} {key}"));
                    self.write(domain, key, value);
                    Some(Ran::ok(""))
                }
                ("/usr/bin/pgrep", ["-u", _, "-x", process]) => {
                    let pids: String = self
                        .running
                        .iter()
                        .filter(|(name, _, _)| name == process)
                        .map(|(_, pid, _)| format!("{pid}\n"))
                        .collect();
                    Some(if pids.is_empty() {
                        Ran {
                            status: Some(1),
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        }
                    } else {
                        Ran::ok(&pids)
                    })
                }
                ("/bin/launchctl", ["print", _]) => Some(if self.session_failures > 0 {
                    self.session_failures -= 1;
                    Ran::exit(2, "injected session lookup failure")
                } else {
                    Ran::ok("")
                }),
                ("/usr/bin/killall", ["-u", _, process]) => {
                    if self.unkillable == Some(*process) {
                        return Some(Ran::exit(2, "injected killall failure"));
                    }
                    self.kill(fake, process);
                    Some(Ran::ok(""))
                }
                _ => None,
            }
        }

        fn kill(&mut self, fake: &Fake, process: &str) {
            let dying: Vec<_> = self
                .running
                .iter()
                .filter(|(name, _, _)| *name == process)
                .cloned()
                .collect();
            self.running.retain(|(name, _, _)| *name != process);
            for (name, pid, starting) in dying {
                if let Some((tiles, _)) = starting {
                    for (key, value) in tiles {
                        self.write(DOCK, key, value);
                    }
                    self.events
                        .push(format!("{name} {pid} wrote back what it read at its start"));
                }
                fake.exited(pid);
                self.events.push(format!("kill {name}"));
                if self.stays_down == Some(name) {
                    continue;
                }
                let pid = self.next_pid;
                self.next_pid += 1;
                let starting = (name == "Dock").then(|| {
                    let tiles = TILES
                        .iter()
                        .filter_map(|key| self.read(DOCK, key).map(|value| (*key, value.clone())))
                        .collect();
                    (tiles, self.start)
                });
                self.running.push((name, pid, starting));
                fake.alive(pid);
            }
        }

        fn tick(&mut self) {
            let mut started = Vec::new();
            for (name, pid, starting) in &mut self.running {
                if let Some((_, left)) = starting {
                    *left = left.saturating_sub(1);
                    if *left == 0
                        && let Some((tiles, _)) = starting.take()
                    {
                        started.push((*name, *pid, tiles));
                    }
                }
            }
            for (name, pid, tiles) in started {
                for (key, value) in tiles {
                    let value = match (&self.managed, key) {
                        (Some(managed), "persistent-apps") => managed.clone(),
                        _ => value,
                    };
                    self.write(DOCK, key, with_guids(value, i64::from(pid)));
                }
                self.events.push(format!("{name} {pid} past its start"));
            }
        }
    }

    fn with_guids(tiles: PValue, guid: i64) -> PValue {
        let PValue::Array(pins) = tiles else {
            return tiles;
        };
        PValue::Array(
            pins.into_iter()
                .map(|pin| match pin {
                    PValue::Dict(mut fields) => {
                        if !fields.iter().any(|(key, _)| key == "GUID") {
                            fields.push(("GUID".into(), PValue::Int(guid)));
                        }
                        PValue::Dict(fields)
                    }
                    other => other,
                })
                .collect(),
        )
    }

    fn desktop_of(who: &User, desk: Desk) -> (Fake, Arc<Mutex<Desk>>) {
        desktop_with(who, desk, true)
    }

    fn desktop_with(who: &User, desk: Desk, dock_apps: bool) -> (Fake, Arc<Mutex<Desk>>) {
        let mut sys = Fake::root();
        sys.euid = who.uid;
        if dock_apps {
            installed_dock_apps(&sys, who);
        }
        for (_, pid, _) in &desk.running {
            sys.alive(*pid);
        }
        let desk = Arc::new(Mutex::new(desk));
        let model = Arc::clone(&desk);
        sys.simulate(move |fake, cmd| model.lock().unwrap().answer(fake, cmd));
        let model = Arc::clone(&desk);
        sys.on_sleep(move |_, _| model.lock().unwrap().tick());
        (sys, desk)
    }

    fn desktop_items(who: &User) -> Vec<Box<dyn Item>> {
        selected_desktop_items(who, &[Setting::Widgets, Setting::Desktop, Setting::Dock])
    }

    fn selected_desktop_items(who: &User, settings: &[Setting]) -> Vec<Box<dyn Item>> {
        selected_desktop_items_at(who, Some(true), settings)
    }

    fn desktop_items_at(who: &User, full_disk_access: Option<bool>) -> Vec<Box<dyn Item>> {
        selected_desktop_items_at(
            who,
            full_disk_access,
            &[Setting::Widgets, Setting::Desktop, Setting::Dock],
        )
    }

    fn selected_desktop_items_at(
        who: &User,
        full_disk_access: Option<bool>,
        settings: &[Setting],
    ) -> Vec<Box<dyn Item>> {
        let mut selected = Selection::none();
        for &setting in settings {
            selected.set(setting, true);
        }
        let mut context = ctx(std::slice::from_ref(who));
        context.facts.full_disk_access = full_disk_access;
        selected_items(&context, selected).unwrap()
    }

    fn journaled(sys: &Fake) -> Vec<(String, String)> {
        String::from_utf8(sys.written(Fake::JOURNAL).unwrap())
            .unwrap()
            .lines()
            .map(|line| {
                let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
                (
                    line["id"].as_str().unwrap().to_owned(),
                    line["to"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn verified(pass: &crate::init::WorkstationRun) -> Vec<(&str, bool)> {
        pass.attempts
            .iter()
            .map(|attempt| (attempt.id.as_str(), attempt.verified))
            .collect()
    }

    fn pass(sys: &Fake, items: Vec<Box<dyn Item>>, mode: Mode) -> crate::init::WorkstationRun {
        Workstation::of(items).run_recording(sys, mode).unwrap()
    }

    fn outcomes(pass: &crate::init::WorkstationRun) -> Vec<(&str, Outcome)> {
        pass.rows
            .iter()
            .map(|row| (row.id.as_str(), row.outcome))
            .collect()
    }

    const WIDGET_IDS: [&str; 3] = [
        "consumer.widgets.desktop@alice",
        "consumer.widgets.stage-manager@alice",
        NC_ID,
    ];

    const NC_ID: &str = "consumer.widgets.notification-center@alice";

    const DESKTOP_IDS: [&str; 5] = [
        WIDGET_IDS[0],
        WIDGET_IDS[1],
        WIDGET_IDS[2],
        "init.desktop@alice",
        "init.dock@alice",
    ];

    #[test]
    fn inits_widget_items_are_the_catalogs_rows_and_the_clear_reads_back() {
        let who = user("alice", 501);
        let mut context = ctx(std::slice::from_ref(&who));
        context.facts.full_disk_access = Some(true);
        let fleet = crate::items::catalog::expand(
            &crate::items::catalog::Table {
                widgets: Some(false),
                ..crate::items::catalog::Table::default()
            },
            &context,
        )
        .unwrap();
        let init = selected_desktop_items(&who, &[Setting::Widgets]);
        let ids = |items: &[Box<dyn Item>]| items.iter().map(|item| item.id()).collect::<Vec<_>>();
        assert_eq!(ids(&init), ids(&fleet));
        assert_eq!(ids(&init), WIDGET_IDS);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let keys = |item: &dyn Item| -> Vec<(String, String)> {
            item.preference_changes(&sys, &context)
                .unwrap()
                .into_iter()
                .map(|(target, _)| (target.domain, target.key))
                .collect()
        };
        let rows: Vec<&CatalogRow> = CATALOG
            .iter()
            .filter(|row| row.preset == "widgets")
            .collect();
        assert_eq!(rows.len(), WIDGET_IDS.len());
        for ((row, init), fleet) in rows.iter().zip(&init).zip(&fleet) {
            let own = vec![(row.domain.to_owned(), row.key.to_owned())];
            assert_eq!(keys(init.as_ref()), own, "{}", init.id());
            assert_eq!(keys(fleet.as_ref()), own, "{}", fleet.id());
        }
        desk.lock().unwrap().write(
            NOTIFICATION_CENTER,
            "widgets",
            PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("future".into(), PValue::Bool(true)),
                (
                    "instances".into(),
                    PValue::Array(vec![PValue::Data(vec![42])]),
                ),
            ]),
        );
        let run = pass(&sys, init, Mode::Apply);
        assert_eq!(
            outcomes(&run),
            WIDGET_IDS.map(|id| (id, Outcome::Changed)),
            "{:?}",
            run.rows
        );
        assert!(run.attempts.iter().all(|attempt| attempt.verified));
        let desk = desk.lock().unwrap();
        let Some(PValue::Dict(list)) = desk.read(NOTIFICATION_CENTER, "widgets") else {
            panic!("{:?}", desk.read(NOTIFICATION_CENTER, "widgets"));
        };
        assert!(
            list.contains(&("instances".into(), PValue::Array(Vec::new())))
                && list.contains(&("future".into(), PValue::Bool(true))),
            "{list:?}"
        );
        assert_eq!(
            desk.count("kill NotificationCenter"),
            1,
            "{:?}",
            desk.events
        );
    }

    #[test]
    fn a_widgets_row_already_set_reads_ok_beside_one_that_begins_the_shared_marker() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        desk.lock().unwrap().write(
            WINDOW_MANAGER,
            "StageManagerHideWidgets",
            PValue::Bool(true),
        );
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Ok),
                (WIDGET_IDS[2], Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        assert!(
            journaled(&sys).iter().all(|(id, _)| id != WIDGET_IDS[1]),
            "{:?}",
            journaled(&sys)
        );
        assert_eq!(desk.lock().unwrap().count("kill Dock"), 1);
        assert!(!crate::init_activation::pending(&sys, &who, WIDGETS).unwrap());

        let (sys, desk) = desktop_of(&who, Desk::before());
        for key in ["StandardHideWidgets", "StageManagerHideWidgets"] {
            desk.lock()
                .unwrap()
                .write(WINDOW_MANAGER, key, PValue::Bool(true));
        }
        crate::init_activation::begin(&sys, &who, WIDGETS).unwrap();
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&run)[..2],
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        for row in &run.rows[..2] {
            assert!(
                row.from
                    .as_deref()
                    .is_some_and(|from| from.contains("after an earlier write")),
                "{row:?}"
            );
        }
        assert_eq!(desk.lock().unwrap().count("kill Dock"), 1);
        assert!(!crate::init_activation::pending(&sys, &who, WIDGETS).unwrap());
    }

    #[test]
    fn an_initially_correct_dock_is_rechecked_after_another_desktop_item_relaunches_it() {
        for (change_desktop, managed) in [(false, true), (true, false), (true, true)] {
            let who = user("alice", 501);
            let settings = [Setting::Desktop, Setting::Dock];
            let (sys, desk) = desktop_of(&who, Desk::before());
            let first = pass(&sys, selected_desktop_items(&who, &settings), Mode::Apply);
            assert!(first.rows.iter().all(|row| row.outcome == Outcome::Changed));
            let recorded = journaled(&sys).len();
            {
                let mut state = desk.lock().unwrap();
                state.events.clear();
                if change_desktop {
                    state.write(
                        WINDOW_MANAGER,
                        "EnableStandardClickToShowDesktop",
                        PValue::Bool(true),
                    );
                }
                state.managed = managed.then(|| PValue::Array(vec![pin("Mail")]));
            }
            let run = pass(&sys, selected_desktop_items(&who, &settings), Mode::Apply);
            let after = pass(&sys, selected_desktop_items(&who, &settings), Mode::Plan);
            let repinned = change_desktop && managed;
            assert_eq!(
                after.rows[1].outcome,
                if repinned {
                    Outcome::Planned
                } else {
                    Outcome::Ok
                },
                "{:?}",
                after.rows
            );
            {
                let state = desk.lock().unwrap();
                assert_eq!(state.count("kill Dock"), usize::from(change_desktop));
                assert!(
                    !state
                        .events
                        .iter()
                        .any(|e| e.starts_with(&format!("write {DOCK}"))),
                    "{:?}",
                    state.events
                );
                assert_eq!(
                    same_apps(
                        &sys,
                        state.read(DOCK, "persistent-apps").unwrap(),
                        &dock_apps(&usual_dock_apps())
                    ),
                    !repinned
                );
            }
            assert!(
                journaled(&sys)[recorded..]
                    .iter()
                    .all(|(id, _)| id != "init.dock@alice"),
                "{:?}",
                journaled(&sys)
            );
            assert!(
                run.attempts
                    .iter()
                    .all(|attempt| attempt.id != "init.dock@alice")
            );
            assert_eq!(
                outcomes(&run),
                [
                    (
                        "init.desktop@alice",
                        if change_desktop {
                            Outcome::Changed
                        } else {
                            Outcome::Ok
                        }
                    ),
                    (
                        "init.dock@alice",
                        if repinned {
                            Outcome::Failed
                        } else {
                            Outcome::Ok
                        }
                    ),
                ],
                "{:?}",
                run.rows
            );
        }
    }

    #[test]
    fn a_dock_that_cannot_pin_aterm_leaves_the_other_desktop_rows_to_apply() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_with(&who, Desk::before(), false);
        let mut first = Workstation::of(desktop_items(&who));
        let snapshot = first.preference_changes(&sys).unwrap();
        assert!(
            snapshot.iter().all(|(target, _)| target.domain != DOCK),
            "{snapshot:?}"
        );
        let run = first.run_recording(&sys, Mode::Apply).unwrap();
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Unknown),
            ],
            "{:?}",
            run.rows
        );
        assert_eq!(
            run.rows[4].why.as_deref(),
            Some(
                "aterm is not installed at /Applications/aterm.app or \
                 /Users//alice/Applications/aterm.app; Dock left untouched"
            ),
            "{:?}",
            run.rows[4]
        );
        assert_eq!(
            verified(&run),
            [
                (WIDGET_IDS[0], true),
                (WIDGET_IDS[1], true),
                (WIDGET_IDS[2], true),
                ("init.desktop@alice", true)
            ]
        );
        assert!(!crate::init_activation::pending(&sys, &who, "dock").unwrap());
        let desk = desk.lock().unwrap();
        assert!(
            !desk
                .events
                .iter()
                .any(|e| e.starts_with(&format!("write {DOCK}"))),
            "{:?}",
            desk.events
        );
        let Some(PValue::Array(pins)) = desk.read(DOCK, "persistent-apps") else {
            panic!("{:?}", desk.read(DOCK, "persistent-apps"));
        };
        let labels: Vec<&PValue> = pins
            .iter()
            .filter_map(|pin| field(field(pin, "tile-data")?, "file-label"))
            .collect();
        assert_eq!(
            labels,
            [&PValue::Str("Mail".into()), &PValue::Str("Music".into())]
        );
        assert_eq!(
            desk.read(WINDOW_MANAGER, "StandardHideWidgets"),
            Some(&PValue::Bool(true))
        );
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter")
            ),
            (1, 1, 1),
            "{:?}",
            desk.events
        );
    }

    #[test]
    fn one_pass_writes_every_desktop_key_before_it_relaunches_each_reader_once() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            DESKTOP_IDS.map(|id| (id, Outcome::Changed)),
            "{:?}",
            run.rows
        );
        assert!(run.attempts.iter().all(|attempt| attempt.verified));
        let desk = desk.lock().unwrap();
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter")
            ),
            (1, 1, 1),
            "{:?}",
            desk.events
        );
        let first_kill = desk
            .events
            .iter()
            .position(|e| e.starts_with("kill "))
            .unwrap();
        let last_write = desk
            .events
            .iter()
            .rposition(|e| e.starts_with("write "))
            .unwrap();
        assert!(last_write < first_kill, "{:?}", desk.events);
        assert_eq!(
            desk.events
                .iter()
                .filter(|e| e.starts_with("Dock ") && e.ends_with(" past its start"))
                .count(),
            1,
            "{:#?}",
            desk.events
        );
        assert!(same_apps(
            &sys,
            desk.read(DOCK, "persistent-apps").unwrap(),
            &dock_apps(&usual_dock_apps())
        ));
        let ids: Vec<String> = journaled(&sys).into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            [
                WIDGET_IDS[0],
                WIDGET_IDS[1],
                WIDGET_IDS[2],
                "init.desktop@alice",
                "init.dock@alice",
                "init.relaunch@alice"
            ]
        );
        for family in [WIDGETS, "desktop", "dock"] {
            assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        drop(desk);

        let (sys, desk) = desktop_of(&who, Desk::before());
        let pins = Target::user(DOCK, "persistent-apps", &who);
        defaults::relaunch(&sys, &who, defaults::Session::Ask, "Dock").unwrap();
        sys.run(&pins.write_cmd(&dock_apps(&usual_dock_apps())))
            .unwrap();
        defaults::relaunch(&sys, &who, defaults::Session::Ask, "Dock").unwrap();
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.read(DOCK, "persistent-apps"),
            Some(&PValue::Array(vec![pin("Mail"), pin("Music")])),
            "{:?}",
            desk.events
        );
    }

    /// `sys` answering as `who`, logged in at this Mac's console, the way
    /// `init::prepare_workstation_selected` resolves the account.
    fn at_console(sys: &Fake, who: &User) {
        sys.logged_in(who);
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: who.uid,
                gid: who.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
    }

    /// One Workstation pass as setup and the doctor build and run it
    /// (`init::prepare_workstation_selected`) with the widgets, desktop and Dock all to change:
    /// each reader relaunches once and every item reads back changed, tagged with its own setting.
    #[test]
    fn a_tagged_pass_relaunches_each_reader_once() {
        let who = user("alice", 501);
        let settings = [Setting::Widgets, Setting::Desktop, Setting::Dock];
        let mut selected = Selection::none();
        for setting in settings {
            selected.set(setting, true);
        }
        let (mut sys, desk) = desktop_of(&who, Desk::before());
        // With Full Disk Access, as the control's items have it: without it the widgets
        // item leaves Notification Center alone.
        sys.full_disk_access = Some(true);
        at_console(&sys, &who);
        let mut prepared = crate::init::prepare_workstation_selected(
            &sys,
            crate::init::WorkstationGroup::All,
            selected,
        )
        .unwrap();
        prepared.preference_changes(&sys).unwrap();
        let run = prepared.run_recording(&sys, Mode::Apply).unwrap();
        assert_eq!(
            outcomes(&run),
            DESKTOP_IDS.map(|id| (id, Outcome::Changed)),
            "{:?}",
            run.rows
        );
        assert!(run.attempts.iter().all(|attempt| attempt.verified));
        let tagged: Vec<(Setting, &str)> = run
            .tagged()
            .into_iter()
            .map(|(setting, row)| (setting, row.id.as_str()))
            .collect();
        let mut want = vec![Setting::Widgets; WIDGET_IDS.len()];
        want.extend([Setting::Desktop, Setting::Dock]);
        assert_eq!(
            tagged,
            want.into_iter().zip(DESKTOP_IDS).collect::<Vec<_>>()
        );
        let desk = desk.lock().unwrap();
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter")
            ),
            (1, 1, 1),
            "{:?}",
            desk.events
        );
    }

    /// One whole desktop pass (widgets, desktop and Dock, at the console) waits in this process
    /// and reads each preference domain once per reading: one write per changed key, no `sleep`
    /// child, one Dock relaunch without a `launchctl print`, no command for the account or the Mac,
    /// no full flush; a pass with nothing to change writes and relaunches nothing. Before
    /// (05b421fb69) the same pass ran 2 `sleep` children, 85 exports, 35 full flushes,
    /// 3 `launchctl print`s and 5 context
    /// commands, and the no-op pass 45 exports.
    ///
    /// Negative control: repeat a write in `Desktop::apply`; the extra write has no matching
    /// snapshot entry and the comparison fails, even though the final preference value is right.
    #[test]
    fn a_whole_desktop_pass_waits_in_process_and_reads_each_domain_once_per_reading() {
        let who = user("alice", 501);
        let mut selected = Selection::none();
        for setting in [Setting::Widgets, Setting::Desktop, Setting::Dock] {
            selected.set(setting, true);
        }
        let (mut sys, desk) = desktop_of(&who, Desk::before());
        sys.full_disk_access = Some(true);
        at_console(&sys, &who);
        let count =
            |calls: &[String], what: &str| calls.iter().filter(|c| c.contains(what)).count();
        for (changes, exports) in [(true, 34), (false, 18)] {
            let before = sys.calls().len();
            let before_events = desk.lock().unwrap().events.len();
            let mut prepared = crate::init::prepare_workstation_selected(
                &sys,
                crate::init::WorkstationGroup::All,
                selected,
            )
            .unwrap();
            let preferences = prepared.preference_changes(&sys).unwrap();
            let run = prepared.run_recording(&sys, Mode::Apply).unwrap();
            let expected = if changes {
                Outcome::Changed
            } else {
                Outcome::Ok
            };
            assert!(
                outcomes(&run)
                    .iter()
                    .all(|(_, outcome)| *outcome == expected),
                "{:?}",
                outcomes(&run)
            );
            let calls = &sys.calls()[before..];
            assert_eq!(count(calls, "/bin/sleep"), 0, "{calls:?}");
            assert_eq!(count(calls, "killall -u alice Dock"), usize::from(changes));
            assert_eq!(count(calls, "launchctl print"), 0, "{calls:?}");
            assert_eq!(count(calls, "durable-dir"), 0, "{calls:?}");
            for context in [
                "/usr/bin/id",
                "/usr/bin/dscl",
                "/usr/bin/uname",
                "/usr/bin/sw_vers",
            ] {
                assert_eq!(count(calls, context), 0, "{calls:?}");
            }
            assert_eq!(count(calls, "identity"), 1, "{calls:?}");
            let read =
                count(calls, "defaults export") + count(calls, "defaults -currentHost export");
            assert!(read <= exports, "{read} exports: {calls:?}");
            // The work grows with the selected preferences. Compare the actual writes to the
            // snapshot instead of counting a fixed number of commands from an older selection.
            let mut expected_writes: Vec<_> = preferences
                .iter()
                .map(|(target, _)| format!("write {} {}", target.domain, target.key))
                .collect();
            let mut writes: Vec<_> = desk.lock().unwrap().events[before_events..]
                .iter()
                .filter(|event| event.starts_with("write "))
                .cloned()
                .collect();
            expected_writes.sort_unstable();
            writes.sort_unstable();
            assert_eq!(writes, expected_writes, "one write per changed key");
        }
    }

    #[test]
    fn the_read_back_waits_until_the_relaunched_dock_is_past_its_start() {
        let who = user("alice", 501);
        let mut managed = Desk::before();
        managed.managed = Some(PValue::Array(vec![pin("Mail")]));
        let (sys, _desk) = desktop_of(&who, managed);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ]
        );
        assert_eq!(
            run.rows[4].why.as_deref(),
            Some("applied, but it still reads com.apple.dock persistent-apps")
        );
        assert_eq!(
            verified(&run),
            [
                (WIDGET_IDS[0], true),
                (WIDGET_IDS[1], true),
                (WIDGET_IDS[2], true),
                ("init.desktop@alice", true),
                ("init.dock@alice", false)
            ]
        );
        let mut slow = Desk::before();
        slow.managed = Some(PValue::Array(vec![pin("Mail")]));
        slow.start = 3;
        let (sys, desk) = desktop_of(&who, slow);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ],
            "{:#?}",
            desk.lock().unwrap().events
        );
        assert!(desk.lock().unwrap().starting().is_empty());
    }

    #[test]
    fn a_pass_that_ends_between_its_writes_and_the_relaunch_relaunches_on_the_next() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let c = ctx(std::slice::from_ref(&who));
        for item in desktop_items(&who) {
            item.apply(&sys, &c).unwrap();
        }
        for family in [WIDGETS, "desktop", "dock"] {
            assert!(crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        let writes = {
            let desk = desk.lock().unwrap();
            assert!(!desk.events.iter().any(|e| e.starts_with("kill ")));
            desk.events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count()
        };
        let checked = pass(&sys, desktop_items(&who), Mode::Plan);
        assert_eq!(
            outcomes(&checked),
            DESKTOP_IDS.map(|id| (id, Outcome::Planned))
        );
        let next = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&next),
            DESKTOP_IDS.map(|id| (id, Outcome::Changed))
        );
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count(),
            writes
        );
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter")
            ),
            (1, 1, 1)
        );
        for family in [WIDGETS, "desktop", "dock"] {
            assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
        }
    }

    #[test]
    fn a_relaunch_that_fails_keeps_the_writes_pending_for_the_next_pass() {
        let who = user("alice", 501);
        let mut failing = Desk::before();
        failing.session_failures = 1;
        let (sys, desk) = desktop_of(&who, failing);
        let first = pass(&sys, desktop_items(&who), Mode::Apply);
        let but_center = |center: Outcome, rest: Outcome| {
            DESKTOP_IDS.map(|id| (id, if id == NC_ID { center } else { rest }))
        };
        assert_eq!(
            outcomes(&first),
            but_center(Outcome::Changed, Outcome::Failed)
        );
        for row in first.rows.iter().filter(|row| row.id != NC_ID) {
            let why = row.why.as_deref().unwrap();
            assert!(
                why.starts_with("applied, but not activated: /bin/launchctl print gui/501"),
                "{why}"
            );
        }
        for family in [WIDGETS, "desktop", "dock"] {
            assert!(crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        assert!(
            !crate::init_activation::pending(&sys, &who, crate::init_activation::CONTAINED_MARKER)
                .unwrap()
        );
        let (writes, kills) = {
            let desk = desk.lock().unwrap();
            let kills = (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter"),
            );
            assert_eq!(kills, (0, 1, 1), "{:?}", desk.events);
            assert_eq!(desk.starting(), []);
            let writes = desk
                .events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count();
            (writes, kills)
        };
        assert!(writes > 0);
        let checked = pass(&sys, desktop_items(&who), Mode::Plan);
        assert_eq!(
            outcomes(&checked),
            but_center(Outcome::Ok, Outcome::Planned)
        );
        let recovered = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&recovered),
            but_center(Outcome::Ok, Outcome::Changed)
        );
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count(),
            writes
        );
        assert_eq!(
            (
                desk.count("kill Dock") - kills.0,
                desk.count("kill Finder") - kills.1,
                desk.count("kill NotificationCenter") - kills.2
            ),
            (1, 1, 0)
        );
    }

    #[test]
    fn a_reader_that_does_not_start_again_fails_only_the_families_whose_keys_it_reads() {
        let who = user("alice", 501);
        let mut down = Desk::before();
        down.stays_down = Some("NotificationCenter");
        let (sys, desk) = desktop_of(&who, down);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Failed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        assert_eq!(
            run.rows[2].why.as_deref(),
            Some(
                "applied, but not activated: NotificationCenter has not started again a \
                 minute after its relaunch; activation remains pending"
            )
        );
        assert_eq!(
            verified(&run),
            [
                (WIDGET_IDS[0], true),
                (WIDGET_IDS[1], true),
                (WIDGET_IDS[2], false),
                ("init.desktop@alice", true),
                ("init.dock@alice", true)
            ]
        );
        for (family, pending) in [
            (WIDGETS, false),
            (crate::init_activation::CONTAINED_MARKER, true),
            ("desktop", false),
            ("dock", false),
        ] {
            assert_eq!(
                crate::init_activation::pending(&sys, &who, family).unwrap(),
                pending,
                "{family}"
            );
        }
        let desk = desk.lock().unwrap();
        assert!(same_apps(
            &sys,
            desk.read(DOCK, "persistent-apps").unwrap(),
            &dock_apps(&usual_dock_apps())
        ));
        assert_eq!(desk.count("kill Dock"), 1, "{:?}", desk.events);
    }

    #[test]
    fn a_relaunch_that_cannot_be_made_still_waits_for_the_readers_already_relaunched() {
        let who = user("alice", 501);
        let mut stuck = Desk::before();
        stuck.unkillable = Some("Finder");
        stuck.start = 2;
        let (sys, desk) = desktop_of(&who, stuck);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Failed),
                ("init.dock@alice", Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        let why = run.rows[3].why.as_deref().unwrap();
        assert!(
            why.starts_with("applied, but not activated: ")
                && why.contains("killall -u alice Finder"),
            "{why}"
        );
        assert!(crate::init_activation::pending(&sys, &who, "desktop").unwrap());
        assert_eq!(desk.lock().unwrap().starting(), []);
        let icons = Target::user(WINDOW_MANAGER, "StandardHideDesktopIcons", &who);
        sys.run(&icons.write_cmd(&PValue::Bool(true))).unwrap();
        defaults::relaunch(&sys, &who, defaults::Session::Ask, "Dock").unwrap();
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.read(WINDOW_MANAGER, "StandardHideDesktopIcons"),
            Some(&PValue::Bool(true))
        );
        assert!(
            !desk
                .events
                .iter()
                .any(|e| e.ends_with("wrote back what it read at its start")),
            "{:#?}",
            desk.events
        );
        assert!(same_apps(
            &sys,
            desk.read(DOCK, "persistent-apps").unwrap(),
            &dock_apps(&usual_dock_apps())
        ));
    }

    #[test]
    fn a_family_whose_write_fails_part_way_is_relaunched_for_what_it_wrote() {
        let who = user("alice", 501);
        let mut partial = Desk::before();
        partial.unwritable = Some((DOCK, "static-apps"));
        let (sys, desk) = desktop_of(&who, partial);
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Widgets, Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ],
            "{:?}",
            run.rows
        );
        assert!(
            run.rows[3]
                .why
                .as_deref()
                .is_some_and(|why| why.contains("static-apps")),
            "{:?}",
            run.rows[3]
        );
        {
            let desk = desk.lock().unwrap();
            let wrote = desk
                .events
                .iter()
                .position(|e| e == "write com.apple.dock persistent-apps")
                .unwrap();
            let killed = desk.events.iter().position(|e| e == "kill Dock").unwrap();
            assert!(wrote < killed, "{:?}", desk.events);
        }
        assert!(!crate::init_activation::pending(&sys, &who, "dock").unwrap());
        let relaunch = journaled(&sys)
            .into_iter()
            .find(|(id, _)| id == "init.relaunch@alice")
            .unwrap();
        assert!(relaunch.1.contains("init.dock@alice"), "{relaunch:?}");
    }

    fn touches_notification_center(call: &str) -> bool {
        call.contains(NOTIFICATION_CENTER) || call.contains("NotificationCenter")
    }

    #[test]
    fn without_full_disk_access_a_pass_never_touches_notification_center_and_settles() {
        let who = user("alice", 501);
        for unknown in [None, Some(false)] {
            let (sys, desk) = desktop_of(&who, Desk::before());
            let mut first = Workstation::of(desktop_items_at(&who, unknown));
            let snapshot = first.preference_changes(&sys).unwrap();
            assert!(
                snapshot
                    .iter()
                    .all(|(target, _)| target.domain != NOTIFICATION_CENTER),
                "{snapshot:?}"
            );
            let run = first.run_recording(&sys, Mode::Apply).unwrap();
            assert_eq!(
                outcomes(&run),
                DESKTOP_IDS.map(|id| (
                    id,
                    if id == NC_ID {
                        Outcome::Ok
                    } else {
                        Outcome::Changed
                    }
                )),
                "{:?}",
                run.rows
            );
            assert_eq!(run.withheld, [NC_ID]);
            let widgets = &run.rows[2];
            assert_eq!(
                widgets.title,
                "No Notification Center widgets; they wait for Full Disk Access"
            );
            assert!(
                widgets
                    .to
                    .as_deref()
                    .is_some_and(|to| to.contains("com.apple.notificationcenterui")),
                "{widgets:?}"
            );
            let calls = sys.calls();
            assert!(
                !calls.iter().any(|call| touches_notification_center(call)),
                "{calls:#?}"
            );
            {
                let desk = desk.lock().unwrap();
                assert_eq!(
                    (
                        desk.count("kill Dock"),
                        desk.count("kill Finder"),
                        desk.count("kill NotificationCenter")
                    ),
                    (1, 1, 0),
                    "{:?}",
                    desk.events
                );
                assert_eq!(
                    desk.read(WINDOW_MANAGER, "StandardHideWidgets"),
                    Some(&PValue::Bool(true))
                );
                let PValue::Dict(widgets) = desk.read(NOTIFICATION_CENTER, "widgets").unwrap()
                else {
                    unreachable!()
                };
                assert!(
                    widgets.iter().any(|(key, value)| key == "instances"
                        && *value == PValue::Array(vec![PValue::Data(vec![42])])),
                    "{widgets:?}"
                );
            }
            for family in [WIDGETS, "desktop", "dock"] {
                assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
            }
            let lines = journaled(&sys).len();
            let (events, calls) = (desk.lock().unwrap().events.len(), sys.calls().len());
            for mode in [Mode::Plan, Mode::Apply] {
                let again = pass(&sys, desktop_items_at(&who, unknown), mode);
                assert_eq!(
                    outcomes(&again),
                    DESKTOP_IDS.map(|id| (id, Outcome::Ok)),
                    "{mode:?}: {:?}",
                    again.rows
                );
            }
            assert_eq!(
                desk.lock().unwrap().events.len(),
                events,
                "{:?}",
                desk.lock().unwrap().events
            );
            assert_eq!(journaled(&sys).len(), lines);
            let later = &sys.calls()[calls..];
            assert!(
                !later.iter().any(|call| touches_notification_center(call)
                    || call.contains("killall")
                    || call.contains(" write ")),
                "{later:#?}"
            );
        }

        let (sys, desk) = desktop_of(&who, Desk::before());
        let run = pass(&sys, desktop_items_at(&who, Some(true)), Mode::Apply);
        assert_eq!(
            run.rows
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>()[..3],
            [
                "No desktop widgets",
                "No Stage Manager widgets",
                "No Notification Center widgets"
            ]
        );
        assert!(run.withheld.is_empty());
        assert!(
            sys.calls()
                .iter()
                .any(|call| touches_notification_center(call))
        );
        assert!(sys.calls().contains(
            &"alice$ /usr/bin/defaults export com.apple.notificationcenterui -".to_owned()
        ));
        assert_eq!(desk.lock().unwrap().count("kill NotificationCenter"), 1);
    }

    #[test]
    fn a_pass_that_reaches_a_container_it_cannot_read_never_settles_and_one_that_waits_does() {
        let who = user("alice", 501);
        let mut hidden = Desk::before();
        hidden.denied = Some(NOTIFICATION_CENTER);
        let (sys, desk) = desktop_of(&who, hidden);
        for first in [true, false] {
            let run = pass(
                &sys,
                selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
                Mode::Apply,
            );
            let window_manager = if first { Outcome::Changed } else { Outcome::Ok };
            assert_eq!(
                outcomes(&run),
                [
                    (WIDGET_IDS[0], window_manager),
                    (WIDGET_IDS[1], window_manager),
                    (NC_ID, Outcome::Failed)
                ]
            );
            assert_eq!(
                run.rows[2].why.as_deref(),
                Some("applied, but it still reads com.apple.notificationcenterui widgets")
            );
        }
        assert_eq!(desk.lock().unwrap().count("kill NotificationCenter"), 2);
        assert_eq!(
            desk.lock()
                .unwrap()
                .count("write com.apple.notificationcenterui widgets"),
            2
        );

        let mut hidden = Desk::before();
        hidden.denied = Some(NOTIFICATION_CENTER);
        let (sys, desk) = desktop_of(&who, hidden);
        let widgets = || selected_desktop_items_at(&who, None, &[Setting::Widgets]);
        let first = pass(&sys, widgets(), Mode::Apply);
        assert_eq!(
            outcomes(&first),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (NC_ID, Outcome::Ok)
            ]
        );
        let second = pass(&sys, widgets(), Mode::Apply);
        assert_eq!(outcomes(&second), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
        let desk = desk.lock().unwrap();
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill NotificationCenter")
            ),
            (1, 0),
            "{:?}",
            desk.events
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| touches_notification_center(call))
        );
    }

    fn follow_up_items(
        who: &User,
        full_disk_access: Option<bool>,
        settings: &[Setting],
    ) -> Vec<Box<dyn Item>> {
        let mut selected = Selection::none();
        for &setting in settings {
            selected.set(setting, true);
        }
        let mut context = ctx(std::slice::from_ref(who));
        context.facts.full_disk_access = full_disk_access;
        withheld_items(&context, selected)
            .unwrap()
            .into_iter()
            .map(|(_, item)| item)
            .collect()
    }

    #[test]
    fn a_follow_up_with_the_grant_clears_notification_center_alone_and_once() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let first = pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&first),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (NC_ID, Outcome::Ok)
            ]
        );
        assert_eq!(first.withheld, [NC_ID]);
        desk.lock()
            .unwrap()
            .write(WINDOW_MANAGER, "StandardHideWidgets", PValue::Bool(false));

        let calls = sys.calls().len();
        let waiting = pass(
            &sys,
            follow_up_items(&who, None, &[Setting::Widgets, Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&waiting), [(NC_ID, Outcome::Ok)]);
        assert_eq!(waiting.withheld, [NC_ID]);
        assert!(
            !sys.calls()[calls..]
                .iter()
                .any(|call| touches_notification_center(call) || call.contains(" write ")),
            "{:#?}",
            &sys.calls()[calls..]
        );

        let calls = sys.calls().len();
        let kills = desk.lock().unwrap().count("kill Dock");
        let follow_up = pass(
            &sys,
            follow_up_items(&who, Some(true), &[Setting::Widgets, Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&follow_up),
            [(NC_ID, Outcome::Changed)],
            "{:?}",
            follow_up.rows
        );
        assert_eq!(follow_up.rows[0].title, "No Notification Center widgets");
        assert!(follow_up.withheld.is_empty());
        {
            let desk = desk.lock().unwrap();
            let PValue::Dict(widgets) = desk.read(NOTIFICATION_CENTER, "widgets").unwrap() else {
                unreachable!()
            };
            assert!(
                widgets
                    .iter()
                    .any(|(key, value)| key == "instances" && *value == PValue::Array(Vec::new())),
                "{widgets:?}"
            );
            assert_eq!(
                desk.read(WINDOW_MANAGER, "StandardHideWidgets"),
                Some(&PValue::Bool(false)),
                "the person's change stays"
            );
            assert_eq!(
                desk.count("kill NotificationCenter"),
                1,
                "{:?}",
                desk.events
            );
            assert_eq!(desk.count("kill Dock"), kills, "{:?}", desk.events);
        }
        assert!(
            !sys.calls()[calls..]
                .iter()
                .any(|call| call.contains(WINDOW_MANAGER) || call.contains(DOCK)),
            "{:#?}",
            &sys.calls()[calls..]
        );
        for marker in [WIDGETS, crate::init_activation::CONTAINED_MARKER] {
            assert!(!crate::init_activation::pending(&sys, &who, marker).unwrap());
        }

        let events = desk.lock().unwrap().events.len();
        let again = pass(
            &sys,
            follow_up_items(&who, Some(true), &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&again), [(NC_ID, Outcome::Ok)]);
        assert_eq!(desk.lock().unwrap().events.len(), events);

        let granted = pass(
            &sys,
            selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
            Mode::Plan,
        );
        assert!(granted.withheld.is_empty());
    }

    #[test]
    fn a_pass_without_the_grant_never_clears_what_notification_center_still_owes() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let mut c = ctx(std::slice::from_ref(&who));
        c.facts.full_disk_access = Some(true);
        for item in selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]) {
            item.apply(&sys, &c).unwrap();
        }
        let owed = |marker: &str| crate::init_activation::pending(&sys, &who, marker).unwrap();
        assert!(owed(WIDGETS) && owed(crate::init_activation::CONTAINED_MARKER));
        assert_eq!(
            desk.lock()
                .unwrap()
                .count("write com.apple.notificationcenterui widgets"),
            1
        );

        let calls = sys.calls().len();
        let waiting = pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&waiting),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (NC_ID, Outcome::Ok)
            ]
        );
        let quiet = pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&quiet), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
        assert!(!owed(WIDGETS));
        assert!(owed(crate::init_activation::CONTAINED_MARKER));
        assert!(
            !sys.calls()[calls..]
                .iter()
                .any(|call| touches_notification_center(call)
                    || call.contains(crate::init_activation::CONTAINED_MARKER)),
            "{:#?}",
            &sys.calls()[calls..]
        );
        {
            let desk = desk.lock().unwrap();
            assert_eq!(
                (
                    desk.count("kill Dock"),
                    desk.count("kill NotificationCenter")
                ),
                (1, 0),
                "{:?}",
                desk.events
            );
        }

        let granted = pass(
            &sys,
            selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&granted),
            [
                (WIDGET_IDS[0], Outcome::Ok),
                (WIDGET_IDS[1], Outcome::Ok),
                (NC_ID, Outcome::Changed)
            ]
        );
        assert!(!owed(crate::init_activation::CONTAINED_MARKER));
        {
            let desk = desk.lock().unwrap();
            assert_eq!(
                (
                    desk.count("kill Dock"),
                    desk.count("kill NotificationCenter"),
                    desk.count("write com.apple.notificationcenterui widgets")
                ),
                (1, 1, 1),
                "{:?}",
                desk.events
            );
        }

        let mut cleared = Desk::before();
        cleared.write(
            NOTIFICATION_CENTER,
            "widgets",
            PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("instances".into(), PValue::Array(Vec::new())),
            ]),
        );
        let (sys, desk) = desktop_of(&who, cleared);
        for item in selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]) {
            item.apply(&sys, &c).unwrap();
        }
        assert!(
            !crate::init_activation::pending(&sys, &who, crate::init_activation::CONTAINED_MARKER)
                .unwrap()
        );
        pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        let granted = pass(
            &sys,
            selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&granted), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
        assert_eq!(desk.lock().unwrap().count("kill NotificationCenter"), 0);
    }

    #[test]
    fn an_older_aterms_widgets_marker_relaunches_notification_center_once_with_the_grant() {
        let who = user("alice", 501);
        let written = || {
            let mut desk = Desk::before();
            for key in ["StandardHideWidgets", "StageManagerHideWidgets"] {
                desk.write(WINDOW_MANAGER, key, PValue::Bool(true));
            }
            desk.write(
                NOTIFICATION_CENTER,
                "widgets",
                PValue::Dict(vec![
                    ("vers".into(), PValue::Int(1)),
                    ("instances".into(), PValue::Array(Vec::new())),
                ]),
            );
            desk
        };
        let kills = |desk: &Arc<Mutex<Desk>>| {
            let desk = desk.lock().unwrap();
            (
                desk.count("kill Dock"),
                desk.count("kill NotificationCenter"),
                desk.events
                    .iter()
                    .filter(|e| e.starts_with("write "))
                    .count(),
            )
        };
        let widgets =
            |granted: Option<bool>| selected_desktop_items_at(&who, granted, &[Setting::Widgets]);
        for marker in [crate::init_activation::LEGACY_WIDGETS_MARKER, WIDGETS] {
            let legacy = marker == crate::init_activation::LEGACY_WIDGETS_MARKER;
            for granted in [Some(true), None] {
                let (sys, desk) = desktop_of(&who, written());
                crate::init_activation::begin(&sys, &who, marker).unwrap();
                let owed =
                    |marker: &str| crate::init_activation::pending(&sys, &who, marker).unwrap();
                let calls = sys.calls().len();
                let first = pass(&sys, widgets(granted), Mode::Apply);
                assert!(
                    granted.is_some()
                        || !sys.calls()[calls..]
                            .iter()
                            .any(|call| touches_notification_center(call)),
                    "{:#?}",
                    &sys.calls()[calls..]
                );
                let center = if legacy && granted.is_some() {
                    Outcome::Changed
                } else {
                    Outcome::Ok
                };
                assert_eq!(
                    outcomes(&first),
                    [
                        (WIDGET_IDS[0], Outcome::Changed),
                        (WIDGET_IDS[1], Outcome::Changed),
                        (NC_ID, center)
                    ],
                    "{marker} {granted:?}: {:?}",
                    first.rows
                );
                let nc_owed = legacy && granted.is_none();
                assert_eq!(
                    kills(&desk),
                    (1, usize::from(legacy && granted.is_some()), 0),
                    "{marker} {granted:?}: {:?}",
                    desk.lock().unwrap().events
                );
                for marker in [crate::init_activation::LEGACY_WIDGETS_MARKER, WIDGETS] {
                    assert!(!owed(marker), "{marker} {granted:?}");
                }
                assert_eq!(
                    owed(crate::init_activation::CONTAINED_MARKER),
                    nc_owed,
                    "{marker} {granted:?}"
                );
                let again = pass(&sys, widgets(Some(true)), Mode::Apply);
                assert_eq!(
                    outcomes(&again),
                    [
                        (WIDGET_IDS[0], Outcome::Ok),
                        (WIDGET_IDS[1], Outcome::Ok),
                        (
                            NC_ID,
                            if nc_owed {
                                Outcome::Changed
                            } else {
                                Outcome::Ok
                            }
                        )
                    ],
                    "{marker} {granted:?}"
                );
                let quiet = pass(&sys, widgets(Some(true)), Mode::Apply);
                assert_eq!(outcomes(&quiet), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
                assert!(!owed(crate::init_activation::CONTAINED_MARKER));
                assert_eq!(
                    kills(&desk),
                    (1, usize::from(legacy), 0),
                    "{marker} {granted:?}: {:?}",
                    desk.lock().unwrap().events
                );
            }
        }
    }
}

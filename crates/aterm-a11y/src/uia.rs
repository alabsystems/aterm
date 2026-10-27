// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows UI Automation delivery for the shared accessibility model.
//!
//! The provider is COM written out over aterm-win32 (WIN-UIA in
//! docs/AUDIT-third-party-2026-10-15.md): [`com`] holds the object machinery,
//! [`objects`] the vtables and their thunks, and this module and [`ranges`] what
//! each method answers, as `Result`s whose failure is the `HRESULT` returned.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use aterm_types::MutexExt;

use aterm_win32::Win32::Foundation::{E_INVALIDARG, HWND};
use aterm_win32::Win32::System::Com::SAFEARRAY;
use aterm_win32::Win32::UI::Accessibility::{
    NavigateDirection, NavigateDirection_FirstChild, NavigateDirection_LastChild,
    NavigateDirection_NextSibling, NavigateDirection_Parent, NavigateDirection_PreviousSibling,
    ProviderOptions, ProviderOptions_ServerSideProvider, StructureChangeType_ChildrenInvalidated,
    ToggleState, ToggleState_Indeterminate, ToggleState_Off, ToggleState_On,
    UIA_AutomationFocusChangedEventId, UIA_ButtonControlTypeId, UIA_CONTROLTYPE_ID,
    UIA_CheckBoxControlTypeId, UIA_ControlTypePropertyId, UIA_DataGridControlTypeId,
    UIA_DocumentControlTypeId, UIA_E_ELEMENTNOTAVAILABLE, UIA_E_ELEMENTNOTENABLED,
    UIA_E_NOTSUPPORTED, UIA_EditControlTypeId, UIA_FrameworkIdPropertyId, UIA_GroupControlTypeId,
    UIA_HasKeyboardFocusPropertyId, UIA_HelpTextPropertyId, UIA_HyperlinkControlTypeId,
    UIA_InvokePatternId, UIA_IsContentElementPropertyId, UIA_IsControlElementPropertyId,
    UIA_IsEnabledPropertyId, UIA_IsKeyboardFocusablePropertyId, UIA_IsOffscreenPropertyId,
    UIA_IsPasswordPropertyId, UIA_ListControlTypeId, UIA_ListItemControlTypeId,
    UIA_LiveRegionChangedEventId, UIA_LiveSettingPropertyId, UIA_MenuControlTypeId,
    UIA_MenuItemControlTypeId, UIA_NamePropertyId, UIA_PATTERN_ID, UIA_PROPERTY_ID,
    UIA_ProgressBarControlTypeId, UIA_RangeValuePatternId, UIA_ScrollItemPatternId,
    UIA_SelectionItemIsSelectedPropertyId, UIA_SelectionItemPatternId, UIA_SelectionPatternId,
    UIA_SliderControlTypeId, UIA_StatusBarControlTypeId, UIA_TabControlTypeId,
    UIA_TabItemControlTypeId, UIA_Text_TextChangedEventId, UIA_Text_TextSelectionChangedEventId,
    UIA_TextControlTypeId, UIA_TextPattern2Id, UIA_TextPatternId, UIA_TogglePatternId,
    UIA_ToggleToggleStatePropertyId, UIA_ValuePatternId, UIA_ValueValuePropertyId,
    UIA_WindowControlTypeId, UiaAppendRuntimeId, UiaDisconnectProvider, UiaHostProviderFromHwnd,
    UiaRaiseAutomationEvent, UiaRaiseAutomationPropertyChangedEvent, UiaRaiseStructureChangedEvent,
    UiaRect, UiaReturnRawElementProvider, UiaRootObjectId,
};
use aterm_win32::com::{Com, IID_IUnknown};
use aterm_win32::core::{BOOL, HRESULT, PCWSTR};
use aterm_win32::uia::{
    IID_IInvokeProvider, IID_IRangeValueProvider, IID_IRawElementProviderFragment,
    IID_IRawElementProviderFragmentRoot, IID_IScrollItemProvider, IID_ISelectionItemProvider,
    IID_ISelectionProvider, IID_ITextProvider2, IID_IToggleProvider, IID_IValueProvider,
};

use crate::model::{
    self, Action, ActionData, ActionHandler, ActivationHandler, Live, Point, Rect, Role, Toggled,
    TreeUpdate,
};
use crate::platform::{Handle, Publication};
use crate::tree::{Node, NodeId, common_filter};

#[path = "uia/com.rs"]
mod com;
#[path = "uia/objects.rs"]
mod objects;
#[path = "uia/ranges.rs"]
mod ranges;
#[cfg(test)]
#[path = "uia/uia_tests.rs"]
mod tests;
#[path = "uia/values.rs"]
mod values;

use com::{Agile, Result};
use values::{Bstr, Variant, array_com, array_i32};

const ELEMENT_NOT_AVAILABLE: HRESULT = UIA_E_ELEMENTNOTAVAILABLE.cast_signed();
const NOT_SUPPORTED: HRESULT = UIA_E_NOTSUPPORTED.cast_signed();
const ELEMENT_NOT_ENABLED: HRESULT = UIA_E_ELEMENTNOTENABLED.cast_signed();

#[derive(Default)]
struct State {
    publication: Publication,
    origin: (f64, f64),
}

struct Shared {
    hwnd: isize,
    state: Mutex<State>,
    root: OnceLock<Agile>,
    active: AtomicBool,
    closed: AtomicBool,
    activation: Mutex<Box<dyn ActivationHandler + Send>>,
    actions: Mutex<Box<dyn ActionHandler + Send>>,
}

static WINDOWS: OnceLock<Mutex<HashMap<isize, Weak<Shared>>>> = OnceLock::new();

fn windows() -> &'static Mutex<HashMap<isize, Weak<Shared>>> {
    WINDOWS.get_or_init(Mutex::default)
}

/// The window's handle as Win32 spells it.
fn hwnd(hwnd: isize) -> HWND {
    std::ptr::without_provenance_mut(hwnd.cast_unsigned())
}

impl Shared {
    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut registry = windows().lock_or_recover();
        if registry
            .get(&self.hwnd)
            .and_then(Weak::upgrade)
            .is_some_and(|live| std::ptr::eq(Arc::as_ptr(&live), self))
        {
            registry.remove(&self.hwnd);
        }
        drop(registry);
        if let Ok(root) = self.root() {
            // SAFETY: `root` is a live provider this window handed out.
            unsafe {
                let _ = UiaDisconnectProvider(root.as_raw());
            }
        }
    }

    /// The fragment root's `IRawElementProviderSimple`, owned.
    fn root(&self) -> Result<Com> {
        self.root
            .get()
            .ok_or(ELEMENT_NOT_AVAILABLE)?
            .query(&aterm_win32::uia::IID_IRawElementProviderSimple)
    }

    #[cfg(test)]
    fn provider(self: &Arc<Self>, id: NodeId) -> Result<Com> {
        let key = Key::capture(&Arc::downgrade(self), id, &self.state.lock_or_recover())?;
        key.provider()
    }

    fn activate(self: &Arc<Self>) {
        if !self.active.swap(true, Ordering::AcqRel) {
            let initial = self.activation.lock_or_recover().request_initial_tree();
            if let Some(update) = initial {
                self.update(update);
            }
        }
    }

    fn update(self: &Arc<Self>, update: TreeUpdate) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let (changes, nodes, announcements) = {
            let mut state = self.state.lock_or_recover();
            let mut events = crate::uia_changes::Events::default();
            let changes = state.publication.update_with_changes(update, &mut events);
            let shared = Arc::downgrade(self);
            let nodes = events
                .nodes
                .into_iter()
                .filter_map(|change| {
                    Key::capture(&shared, change.id, &state)
                        .ok()
                        .map(|key| (key, change))
                })
                .collect::<Vec<_>>();
            let announcements = changes
                .announcements
                .iter()
                .filter_map(|(id, _, _)| Key::capture(&shared, *id, &state).ok())
                .collect::<Vec<_>>();
            (changes, nodes, announcements)
        };
        let Ok(root) = self.root() else {
            return;
        };
        if !changes.added.is_empty() || !changes.removed.is_empty() || !changes.replaced.is_empty()
        {
            // SAFETY: `root` is a live provider; no runtime ID accompanies a
            // ChildrenInvalidated event.
            unsafe {
                let _ = UiaRaiseStructureChangedEvent(
                    root.as_raw(),
                    StructureChangeType_ChildrenInvalidated,
                    std::ptr::null_mut(),
                    0,
                );
            }
        }
        for (key, crate::uia_changes::NodeChange { old, new, .. }) in nodes {
            let Ok(provider) = key.provider() else {
                continue;
            };
            // SAFETY (each raise below): `provider` is a live provider.
            if old.text != new.text && new.text.is_some() {
                unsafe {
                    let _ = UiaRaiseAutomationEvent(provider.as_raw(), UIA_Text_TextChangedEventId);
                }
            }
            if old.selection != new.selection {
                unsafe {
                    let _ = UiaRaiseAutomationEvent(
                        provider.as_raw(),
                        UIA_Text_TextSelectionChangedEventId,
                    );
                }
            }
            let text = |value: &Option<String>| Variant::text(value.as_deref().unwrap_or_default());
            for (changed, property, before, after) in [
                (
                    old.name != new.name,
                    UIA_NamePropertyId,
                    text(&old.name),
                    text(&new.name),
                ),
                (
                    old.description != new.description,
                    UIA_HelpTextPropertyId,
                    text(&old.description),
                    text(&new.description),
                ),
                (
                    old.value != new.value,
                    UIA_ValueValuePropertyId,
                    text(&old.value),
                    text(&new.value),
                ),
                (
                    old.enabled != new.enabled,
                    UIA_IsEnabledPropertyId,
                    Ok(Variant::bool(old.enabled)),
                    Ok(Variant::bool(new.enabled)),
                ),
                (
                    old.selected != new.selected,
                    UIA_SelectionItemIsSelectedPropertyId,
                    Ok(Variant::bool(old.selected.unwrap_or(false))),
                    Ok(Variant::bool(new.selected.unwrap_or(false))),
                ),
                (
                    old.toggled != new.toggled,
                    UIA_ToggleToggleStatePropertyId,
                    Ok(Variant::i32(toggle(old.toggled))),
                    Ok(Variant::i32(toggle(new.toggled))),
                ),
            ] {
                if changed && let (Ok(before), Ok(after)) = (before, after) {
                    // SAFETY: the VARIANTs pass by value and stay owned here,
                    // cleared when `before` and `after` drop.
                    unsafe {
                        let _ = UiaRaiseAutomationPropertyChangedEvent(
                            provider.as_raw(),
                            property,
                            before.by_value(),
                            after.by_value(),
                        );
                    }
                }
            }
        }
        for key in announcements {
            if let Ok(provider) = key.provider() {
                // SAFETY: `provider` is a live provider.
                unsafe {
                    let _ =
                        UiaRaiseAutomationEvent(provider.as_raw(), UIA_LiveRegionChangedEventId);
                }
            }
        }
        if changes.focus {
            self.raise_focus();
        }
    }

    fn raise_focus(self: &Arc<Self>) {
        let key = {
            let state = self.state.lock_or_recover();
            state
                .publication
                .tree()
                .and_then(|tree| tree.focus_id())
                .and_then(|id| Key::capture(&Arc::downgrade(self), id, &state).ok())
        };
        if let Some(key) = key
            && let Ok(provider) = key.provider()
        {
            // SAFETY: `provider` is a live provider.
            unsafe {
                let _ =
                    UiaRaiseAutomationEvent(provider.as_raw(), UIA_AutomationFocusChangedEventId);
            }
        }
    }
}

/// A real window's UIA publisher.
pub struct Adapter {
    shared: Arc<Shared>,
}

impl Adapter {
    /// Attach to a live HWND owned by the caller.
    ///
    /// # Safety
    /// `hwnd` must remain the window's handle until this adapter is dropped.
    pub unsafe fn new(
        hwnd: isize,
        activation: impl ActivationHandler + Send + 'static,
        actions: impl ActionHandler + Send + 'static,
    ) -> Option<Self> {
        if hwnd == 0 {
            return None;
        }
        let mut publication = Publication::default();
        let mut root = model::Node::new(Role::Window);
        root.set_label("aterm");
        publication.update(TreeUpdate {
            nodes: vec![(model::NodeId(0), root)],
            tree: Some(model::Tree::new(model::NodeId(0))),
            tree_id: model::TreeId::ROOT,
            focus: model::NodeId(0),
        });
        let shared = Arc::new(Shared {
            hwnd,
            state: Mutex::new(State {
                publication,
                origin: (0.0, 0.0),
            }),
            root: OnceLock::new(),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            activation: Mutex::new(Box::new(activation)),
            actions: Mutex::new(Box::new(actions)),
        });
        let _ = shared.root.set(Agile::new(objects::provider(Provider {
            shared: Arc::downgrade(&shared),
            handle: None,
        })));
        windows()
            .lock_or_recover()
            .insert(hwnd, Arc::downgrade(&shared));
        Some(Self { shared })
    }

    /// Publish only after a client has requested this window's provider.
    pub fn update_if_active(&mut self, update: impl FnOnce() -> TreeUpdate) {
        if self.shared.active.load(Ordering::Acquire) {
            self.shared.update(update());
        }
    }

    /// Physical screen bounds; node coordinates are relative to the client area.
    pub fn set_root_window_bounds(&mut self, _outer: Rect, inner: Rect) {
        self.shared.state.lock_or_recover().origin = (inner.x0, inner.y0);
    }

    /// Keep the published focus aligned with the native window's focus.
    pub fn update_window_focus_state(&mut self, focused: bool) {
        let changes = self
            .shared
            .state
            .lock_or_recover()
            .publication
            .set_focused(focused);
        if changes.focus {
            self.shared.raise_focus();
        }
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.shared.close();
    }
}

/// Retire the publication as soon as the native HWND is destroyed.
pub fn window_destroyed(hwnd: isize) {
    let shared = windows()
        .lock_or_recover()
        .get(&hwnd)
        .and_then(Weak::upgrade);
    if let Some(shared) = shared {
        shared.close();
    }
}

/// Handle UIA's root-object request from the host's window proc.
pub fn handle_wm_getobject(hwnd: isize, wparam: usize, lparam: isize) -> Option<isize> {
    if lparam as i32 != UiaRootObjectId {
        return None;
    }
    let shared = windows().lock_or_recover().get(&hwnd)?.upgrade()?;
    if shared.closed.load(Ordering::Acquire) {
        return None;
    }
    shared.activate();
    let root = shared.root().ok()?;
    // SAFETY: the window proc's own message parameters, and a live provider,
    // which UIA AddRefs to keep.
    Some(unsafe { UiaReturnRawElementProvider(self::hwnd(hwnd), wparam, lparam, root.as_raw()) })
}

#[derive(Clone)]
pub(super) struct Key {
    shared: Weak<Shared>,
    handle: Option<Handle>,
}

impl Key {
    fn capture(shared: &Weak<Shared>, id: NodeId, state: &State) -> Result<Self> {
        Ok(Self {
            shared: shared.clone(),
            handle: Some(state.publication.handle(id).ok_or(ELEMENT_NOT_AVAILABLE)?),
        })
    }

    fn read<T>(&self, f: impl FnOnce(Node<'_>, &State) -> Result<T>) -> Result<T> {
        let shared = self.shared.upgrade().ok_or(ELEMENT_NOT_AVAILABLE)?;
        if shared.closed.load(Ordering::Acquire) {
            return Err(ELEMENT_NOT_AVAILABLE);
        }
        let state = shared.state.lock_or_recover();
        let node = match &self.handle {
            Some(handle) => state.publication.node(handle),
            None => state.publication.tree().map(crate::tree::State::root),
        }
        .ok_or(ELEMENT_NOT_AVAILABLE)?;
        f(node, &state)
    }

    fn dispatch(&self, action: Action, data: Option<ActionData>) -> Result<()> {
        let request = self.read(|node, state| {
            if node.is_disabled() {
                return Err(ELEMENT_NOT_ENABLED);
            }
            let handle = match &self.handle {
                Some(handle) => handle.clone(),
                None => state
                    .publication
                    .handle(node.id())
                    .ok_or(ELEMENT_NOT_AVAILABLE)?,
            };
            state
                .publication
                .action(&handle, action, data)
                .ok_or(NOT_SUPPORTED)
        })?;
        let shared = self.shared.upgrade().ok_or(ELEMENT_NOT_AVAILABLE)?;
        shared.actions.lock_or_recover().do_action(request);
        Ok(())
    }

    /// A new provider for the node, as `IRawElementProviderSimple`.
    fn provider(&self) -> Result<Com> {
        self.read(|_, _| Ok(()))?;
        Ok(objects::provider(Provider {
            shared: self.shared.clone(),
            handle: self.handle.clone(),
        }))
    }
}

/// One element: the window's fragment root (`handle` `None`) or one node.
pub(super) struct Provider {
    shared: Weak<Shared>,
    handle: Option<Handle>,
}

impl Provider {
    fn key(&self) -> Key {
        Key {
            shared: self.shared.clone(),
            handle: self.handle.clone(),
        }
    }
}

fn control_type(role: Role) -> UIA_CONTROLTYPE_ID {
    match role {
        Role::Window | Role::AlertDialog => UIA_WindowControlTypeId,
        Role::Terminal | Role::Document => UIA_DocumentControlTypeId,
        Role::TextInput | Role::MultilineTextInput | Role::SearchInput => UIA_EditControlTypeId,
        Role::Button => UIA_ButtonControlTypeId,
        Role::Link => UIA_HyperlinkControlTypeId,
        Role::Switch => UIA_CheckBoxControlTypeId,
        Role::Slider => UIA_SliderControlTypeId,
        Role::Tab => UIA_TabItemControlTypeId,
        Role::TabList => UIA_TabControlTypeId,
        Role::List | Role::ListBox => UIA_ListControlTypeId,
        Role::ListBoxOption => UIA_ListItemControlTypeId,
        Role::Menu => UIA_MenuControlTypeId,
        Role::MenuItem | Role::MenuItemRadio => UIA_MenuItemControlTypeId,
        Role::ProgressIndicator | Role::Meter => UIA_ProgressBarControlTypeId,
        Role::Grid => UIA_DataGridControlTypeId,
        Role::Status | Role::Alert => UIA_StatusBarControlTypeId,
        Role::Label | Role::Heading | Role::TextRun => UIA_TextControlTypeId,
        _ => UIA_GroupControlTypeId,
    }
}

fn toggle(value: Option<Toggled>) -> ToggleState {
    match value {
        Some(Toggled::True) => ToggleState_On,
        Some(Toggled::Mixed) => ToggleState_Indeterminate,
        _ => ToggleState_Off,
    }
}

fn boolean(value: bool) -> BOOL {
    BOOL::from(value)
}

/// `IRawElementProviderSimple`.
impl Provider {
    fn provider_options(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    /// The pattern's provider (the object's identity), or none when the node
    /// does not support it.
    fn get_pattern_provider(&self, pattern: UIA_PATTERN_ID) -> Result<Option<Com>> {
        let key = self.key().read(|node, state| {
            let supported = match pattern {
                UIA_TextPatternId | UIA_TextPattern2Id => node.supports_text_ranges(),
                UIA_InvokePatternId => {
                    node.data().supports_action(Action::Click)
                        && node.toggled().is_none()
                        && node.is_selected().is_none()
                }
                UIA_TogglePatternId => {
                    node.toggled().is_some() && node.data().supports_action(Action::Click)
                }
                UIA_ValuePatternId => node.is_text_input(),
                UIA_RangeValuePatternId => node.numeric_value().is_some(),
                UIA_SelectionPatternId => node.is_container_with_selectable_children(),
                UIA_SelectionItemPatternId => {
                    node.is_selected().is_some() && node.data().supports_action(Action::Click)
                }
                UIA_ScrollItemPatternId => node.data().supports_action(Action::ScrollIntoView),
                _ => false,
            };
            if !supported {
                return Ok(None);
            }
            Key::capture(&self.shared, node.id(), state).map(Some)
        })?;
        let Some(key) = key else {
            return Ok(None);
        };
        let provider = key.provider()?;
        let iid = match pattern {
            UIA_TextPatternId | UIA_TextPattern2Id => &IID_ITextProvider2,
            UIA_InvokePatternId => &IID_IInvokeProvider,
            UIA_TogglePatternId => &IID_IToggleProvider,
            UIA_ValuePatternId => &IID_IValueProvider,
            UIA_RangeValuePatternId => &IID_IRangeValueProvider,
            UIA_SelectionPatternId => &IID_ISelectionProvider,
            UIA_SelectionItemPatternId => &IID_ISelectionItemProvider,
            UIA_ScrollItemPatternId => &IID_IScrollItemProvider,
            _ => return Ok(None),
        };
        provider.query(iid)?.query(&IID_IUnknown).map(Some)
    }

    fn get_property_value(&self, id: UIA_PROPERTY_ID) -> Result<Variant> {
        self.key().read(|node, _| {
            Ok(match id {
                UIA_ControlTypePropertyId => Variant::i32(control_type(node.role())),
                UIA_NamePropertyId => Variant::text(&node.label().unwrap_or_default())?,
                UIA_HelpTextPropertyId => Variant::text(&node.description().unwrap_or_default())?,
                UIA_FrameworkIdPropertyId => Variant::text("aterm")?,
                UIA_IsEnabledPropertyId => Variant::bool(!node.is_disabled()),
                UIA_IsOffscreenPropertyId => Variant::bool(node.is_offscreen()),
                UIA_HasKeyboardFocusPropertyId => Variant::bool(node.is_focused()),
                UIA_IsKeyboardFocusablePropertyId => {
                    Variant::bool(node.is_focused() || node.data().supports_action(Action::Focus))
                }
                UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => {
                    Variant::bool(node.role() != Role::TextRun)
                }
                UIA_IsPasswordPropertyId => Variant::bool(false),
                UIA_LiveSettingPropertyId => Variant::i32(match node.live() {
                    Live::Off => 0,
                    Live::Polite => 1,
                    Live::Assertive => 2,
                }),
                _ => Variant::empty(),
            })
        })
    }

    /// The HWND's own provider, for the fragment root only.
    fn host_raw_element_provider(&self) -> Result<Option<Com>> {
        let root = self.key().read(|node, _| Ok(node.is_root()))?;
        if !root {
            return Ok(None);
        }
        let shared = self.shared.upgrade().ok_or(ELEMENT_NOT_AVAILABLE)?;
        let mut out = std::ptr::null_mut();
        // SAFETY: the window's handle; `out` receives an owned provider, or
        // null, on success.
        let hr = unsafe { UiaHostProviderFromHwnd(hwnd(shared.hwnd), &mut out) };
        if hr < 0 {
            return Err(hr);
        }
        // SAFETY: a successful call hands over the reference `out` holds.
        Ok(unsafe { Com::from_raw(out) })
    }
}

/// `IRawElementProviderFragment`.
impl Provider {
    fn navigate(&self, direction: NavigateDirection) -> Result<Option<Com>> {
        let key = self.key().read(|node, state| {
            let id = match direction {
                NavigateDirection_Parent if !node.is_root() => {
                    node.filtered_parent(&common_filter).map(|n| n.id())
                }
                NavigateDirection_NextSibling if !node.is_root() => node
                    .following_filtered_siblings(&common_filter)
                    .next()
                    .map(|n| n.id()),
                NavigateDirection_PreviousSibling if !node.is_root() => node
                    .preceding_filtered_siblings(&common_filter)
                    .next()
                    .map(|n| n.id()),
                NavigateDirection_FirstChild => node
                    .filtered_children(&common_filter)
                    .next()
                    .map(|n| n.id()),
                NavigateDirection_LastChild => node
                    .filtered_children(&common_filter)
                    .next_back()
                    .map(|n| n.id()),
                _ => None,
            };
            id.map(|id| Key::capture(&self.shared, id, state))
                .transpose()
        })?;
        key.map(|key| key.provider()?.query(&IID_IRawElementProviderFragment))
            .transpose()
    }

    /// The runtime ID, or null for the fragment root, which its host names.
    fn get_runtime_id(&self) -> Result<*mut SAFEARRAY> {
        let id = self
            .key()
            .read(|node, _| Ok((!node.is_root()).then(|| runtime_id(node))))?;
        id.map_or(Ok(std::ptr::null_mut()), |id| array_i32(&id))
    }

    fn bounding_rectangle(&self) -> Result<UiaRect> {
        self.key().read(|node, state| {
            Ok(to_uia_rect(
                node.bounding_box().unwrap_or_default(),
                state.origin,
            ))
        })
    }

    fn get_embedded_fragment_roots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn set_focus(&self) -> Result<()> {
        self.key().dispatch(Action::Focus, None)
    }

    fn fragment_root(&self) -> Result<Com> {
        self.key().read(|_, _| Ok(()))?;
        self.shared
            .upgrade()
            .ok_or(ELEMENT_NOT_AVAILABLE)?
            .root()?
            .query(&IID_IRawElementProviderFragmentRoot)
    }
}

fn runtime_id(node: Node<'_>) -> [i32; 5] {
    let id = node.id().0.0;
    let incarnation = node.incarnation();
    [
        UiaAppendRuntimeId as i32,
        (id >> 32) as i32,
        id as i32,
        (incarnation >> 32) as i32,
        incarnation as i32,
    ]
}

/// `IRawElementProviderFragmentRoot`.
impl Provider {
    fn element_provider_from_point(&self, x: f64, y: f64) -> Result<Option<Com>> {
        if !x.is_finite() || !y.is_finite() {
            return Err(E_INVALIDARG);
        }
        let key = self.key().read(|node, state| {
            node.hit_test(
                Point::new(x - state.origin.0, y - state.origin.1),
                &common_filter,
            )
            .map(|(node, _)| Key::capture(&self.shared, node.id(), state))
            .transpose()
        })?;
        key.map(|key| key.provider()?.query(&IID_IRawElementProviderFragment))
            .transpose()
    }

    fn get_focus(&self) -> Result<Option<Com>> {
        let key = self.key().read(|_, state| {
            state
                .publication
                .tree()
                .and_then(|tree| tree.focus_id())
                .map(|id| Key::capture(&self.shared, id, state))
                .transpose()
        })?;
        key.map(|key| key.provider()?.query(&IID_IRawElementProviderFragment))
            .transpose()
    }
}

/// `IInvokeProvider` and `IToggleProvider`.
impl Provider {
    fn invoke(&self) -> Result<()> {
        self.key().dispatch(Action::Click, None)
    }

    fn toggle(&self) -> Result<()> {
        self.key().dispatch(Action::Click, None)
    }

    fn toggle_state(&self) -> Result<ToggleState> {
        self.key().read(|node, _| {
            Ok(match node.toggled().ok_or(NOT_SUPPORTED)? {
                Toggled::True => ToggleState_On,
                Toggled::False => ToggleState_Off,
                Toggled::Mixed => ToggleState_Indeterminate,
            })
        })
    }
}

/// `IValueProvider`.
impl Provider {
    fn set_value(&self, value: PCWSTR) -> Result<()> {
        if value.is_null() {
            return Err(E_INVALIDARG);
        }
        // SAFETY: a non-null string the caller passed, NUL-terminated by
        // IValueProvider::SetValue's contract.
        let value = unsafe { values::wide_text(value) }?;
        self.key().dispatch(
            Action::SetValue,
            Some(ActionData::Value(value.into_boxed_str())),
        )
    }

    fn value(&self) -> Result<Bstr> {
        self.key()
            .read(|node, _| Bstr::new(&crate::platform::text(node).unwrap_or_default()))
    }

    fn is_read_only(&self) -> Result<BOOL> {
        self.key()
            .read(|node, _| Ok(boolean(!node.data().supports_action(Action::SetValue))))
    }
}

/// `IRangeValueProvider`.
impl Provider {
    fn set_range_value(&self, value: f64) -> Result<()> {
        if !value.is_finite() {
            return Err(E_INVALIDARG);
        }
        self.key().read(|node, _| {
            if node.min_numeric_value().is_some_and(|min| value < min)
                || node.max_numeric_value().is_some_and(|max| value > max)
            {
                return Err(E_INVALIDARG);
            }
            Ok(())
        })?;
        self.key()
            .dispatch(Action::SetValue, Some(ActionData::NumericValue(value)))
    }

    fn range_value(&self) -> Result<f64> {
        self.key()
            .read(|node, _| node.numeric_value().ok_or(NOT_SUPPORTED))
    }

    fn maximum(&self) -> Result<f64> {
        self.key()
            .read(|node, _| node.max_numeric_value().ok_or(NOT_SUPPORTED))
    }

    fn minimum(&self) -> Result<f64> {
        self.key()
            .read(|node, _| node.min_numeric_value().ok_or(NOT_SUPPORTED))
    }

    /// Large and small change alike.
    fn change(&self) -> Result<f64> {
        self.key()
            .read(|node, _| Ok(node.numeric_value_step().unwrap_or(0.0)))
    }
}

/// `IScrollItemProvider`, `ISelectionItemProvider` and `ISelectionProvider`.
impl Provider {
    fn scroll_into_view(&self) -> Result<()> {
        self.key().dispatch(Action::ScrollIntoView, None)
    }

    fn select(&self) -> Result<()> {
        self.key().dispatch(Action::Click, None)
    }

    fn is_selected(&self) -> Result<BOOL> {
        self.key()
            .read(|node, _| Ok(boolean(node.is_selected().ok_or(NOT_SUPPORTED)?)))
    }

    fn selection_container(&self) -> Result<Option<Com>> {
        let key = self.key().read(|node, state| {
            node.selection_container(&common_filter)
                .map(|parent| Key::capture(&self.shared, parent.id(), state))
                .transpose()
        })?;
        key.map(|key| key.provider()).transpose()
    }

    fn get_selection(&self) -> Result<*mut SAFEARRAY> {
        let keys = self.key().read(|node, state| {
            node.filtered_children(&common_filter)
                .filter(|n| n.is_selected() == Some(true))
                .map(|n| Key::capture(&self.shared, n.id(), state))
                .collect::<Result<Vec<_>>>()
        })?;
        let values = keys
            .into_iter()
            .map(|key| key.provider()?.query(&IID_IUnknown))
            .collect::<Result<Vec<Com>>>()?;
        array_com(&values)
    }
}

fn to_uia_rect(rect: Rect, origin: (f64, f64)) -> UiaRect {
    UiaRect {
        left: rect.x0 + origin.0,
        top: rect.y0 + origin.1,
        width: (rect.x1 - rect.x0).max(0.0),
        height: (rect.y1 - rect.y0).max(0.0),
    }
}

/// The raw pointer a borrowed interface argument arrives as.
type Raw = *mut c_void;

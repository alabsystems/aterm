// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows UI Automation delivery for the shared accessibility model.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use aterm_types::MutexExt;

use windows::Win32::Foundation::{E_INVALIDARG, E_OUTOFMEMORY, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARENUM, VARIANT, VT_I4, VT_R8, VT_UNKNOWN};
use windows::Win32::UI::Accessibility::*;
use windows::core::{
    BOOL, BSTR, ComObject, Error, HRESULT, IUnknown, Interface, PCWSTR, Ref, Result, implement,
};

use crate::model::{
    self, Action, ActionData, ActionHandler, ActivationHandler, Live, Point, Rect, Role, Toggled,
    TreeUpdate,
};
use crate::platform::{Handle, Publication};
use crate::tree::{Node, NodeId, common_filter};

#[path = "uia/nullable.rs"]
mod nullable;
#[path = "uia/ranges.rs"]
mod ranges;
#[cfg(test)]
#[path = "uia/uia_tests.rs"]
mod tests;

const ELEMENT_NOT_AVAILABLE: HRESULT = HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32);
const NOT_SUPPORTED: HRESULT = HRESULT(UIA_E_NOTSUPPORTED as i32);
const ELEMENT_NOT_ENABLED: HRESULT = HRESULT(UIA_E_ELEMENTNOTENABLED as i32);

fn unavailable() -> Error {
    ELEMENT_NOT_AVAILABLE.into()
}
fn unsupported() -> Error {
    NOT_SUPPORTED.into()
}

#[derive(Default)]
struct State {
    publication: Publication,
    origin: (f64, f64),
}

struct Shared {
    hwnd: isize,
    state: Mutex<State>,
    root: OnceLock<ComObject<Provider>>,
    active: AtomicBool,
    closed: AtomicBool,
    activation: Mutex<Box<dyn ActivationHandler + Send>>,
    actions: Mutex<Box<dyn ActionHandler + Send>>,
}

static WINDOWS: OnceLock<Mutex<HashMap<isize, Weak<Shared>>>> = OnceLock::new();

fn windows() -> &'static Mutex<HashMap<isize, Weak<Shared>>> {
    WINDOWS.get_or_init(Mutex::default)
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
            unsafe {
                let _ = UiaDisconnectProvider(&root);
            }
        }
    }
    fn root(&self) -> Result<IRawElementProviderSimple> {
        self.root
            .get()
            .map(|root| root.to_interface::<nullable::Simple>().0)
            .ok_or_else(unavailable)
    }

    #[cfg(test)]
    fn provider(self: &Arc<Self>, id: NodeId) -> Result<IRawElementProviderSimple> {
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
        unsafe {
            if !changes.added.is_empty()
                || !changes.removed.is_empty()
                || !changes.replaced.is_empty()
            {
                let _ = UiaRaiseStructureChangedEvent(
                    &root,
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
            unsafe {
                if old.text != new.text && new.text.is_some() {
                    let _ = UiaRaiseAutomationEvent(&provider, UIA_Text_TextChangedEventId);
                }
                if old.selection != new.selection {
                    let _ =
                        UiaRaiseAutomationEvent(&provider, UIA_Text_TextSelectionChangedEventId);
                }
                for (changed, property, before, after) in [
                    (
                        old.name != new.name,
                        UIA_NamePropertyId,
                        BSTR::from(old.name.as_deref().unwrap_or_default()).into(),
                        BSTR::from(new.name.as_deref().unwrap_or_default()).into(),
                    ),
                    (
                        old.description != new.description,
                        UIA_HelpTextPropertyId,
                        BSTR::from(old.description.as_deref().unwrap_or_default()).into(),
                        BSTR::from(new.description.as_deref().unwrap_or_default()).into(),
                    ),
                    (
                        old.value != new.value,
                        UIA_ValueValuePropertyId,
                        BSTR::from(old.value.as_deref().unwrap_or_default()).into(),
                        BSTR::from(new.value.as_deref().unwrap_or_default()).into(),
                    ),
                    (
                        old.enabled != new.enabled,
                        UIA_IsEnabledPropertyId,
                        old.enabled.into(),
                        new.enabled.into(),
                    ),
                    (
                        old.selected != new.selected,
                        UIA_SelectionItemIsSelectedPropertyId,
                        old.selected.unwrap_or(false).into(),
                        new.selected.unwrap_or(false).into(),
                    ),
                    (
                        old.toggled != new.toggled,
                        UIA_ToggleToggleStatePropertyId,
                        toggle(old.toggled).0.into(),
                        toggle(new.toggled).0.into(),
                    ),
                ] {
                    if changed {
                        let _ = UiaRaiseAutomationPropertyChangedEvent(
                            &provider, property, &before, &after,
                        );
                    }
                }
            }
        }
        for key in announcements {
            if let Ok(provider) = key.provider() {
                unsafe {
                    let _ = UiaRaiseAutomationEvent(&provider, UIA_LiveRegionChangedEventId);
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
            unsafe {
                let _ = UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId);
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
        let _ = shared.root.set(ComObject::new(Provider {
            shared: Arc::downgrade(&shared),
            handle: None,
        }));
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
    Some(unsafe {
        UiaReturnRawElementProvider(HWND(hwnd as *mut _), WPARAM(wparam), LPARAM(lparam), &root).0
    })
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
            handle: Some(state.publication.handle(id).ok_or_else(unavailable)?),
        })
    }

    fn read<T>(&self, f: impl FnOnce(Node<'_>, &State) -> Result<T>) -> Result<T> {
        let shared = self.shared.upgrade().ok_or_else(unavailable)?;
        if shared.closed.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let state = shared.state.lock_or_recover();
        let node = match &self.handle {
            Some(handle) => state.publication.node(handle),
            None => state.publication.tree().map(crate::tree::State::root),
        }
        .ok_or_else(unavailable)?;
        f(node, &state)
    }

    fn dispatch(&self, action: Action, data: Option<ActionData>) -> Result<()> {
        let request = self.read(|node, state| {
            if node.is_disabled() {
                return Err(ELEMENT_NOT_ENABLED.into());
            }
            let handle = match &self.handle {
                Some(handle) => handle.clone(),
                None => state
                    .publication
                    .handle(node.id())
                    .ok_or_else(unavailable)?,
            };
            state
                .publication
                .action(&handle, action, data)
                .ok_or_else(unsupported)
        })?;
        let shared = self.shared.upgrade().ok_or_else(unavailable)?;
        shared.actions.lock_or_recover().do_action(request);
        Ok(())
    }

    fn provider(&self) -> Result<IRawElementProviderSimple> {
        self.read(|_, _| Ok(()))?;
        Ok(ComObject::new(Provider {
            shared: self.shared.clone(),
            handle: self.handle.clone(),
        })
        .to_interface::<nullable::Simple>()
        .0)
    }
}

#[implement(
    nullable::Simple,
    nullable::Fragment,
    nullable::FragmentRoot,
    IInvokeProvider,
    IValueProvider,
    IRangeValueProvider,
    IToggleProvider,
    ISelectionProvider,
    nullable::SelectionItem,
    IScrollItemProvider,
    ITextProvider2
)]
struct Provider {
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

impl IRawElementProviderSimple_Impl for Provider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
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
                return Err(Error::empty());
            }
            Key::capture(&self.shared, node.id(), state)
        })?;
        let provider = key.provider()?;
        match pattern {
            UIA_TextPatternId | UIA_TextPattern2Id => provider.cast::<ITextProvider2>()?.cast(),
            UIA_InvokePatternId => provider.cast::<IInvokeProvider>()?.cast(),
            UIA_TogglePatternId => provider.cast::<IToggleProvider>()?.cast(),
            UIA_ValuePatternId => provider.cast::<IValueProvider>()?.cast(),
            UIA_RangeValuePatternId => provider.cast::<IRangeValueProvider>()?.cast(),
            UIA_SelectionPatternId => provider.cast::<ISelectionProvider>()?.cast(),
            UIA_SelectionItemPatternId => provider.cast::<ISelectionItemProvider>()?.cast(),
            UIA_ScrollItemPatternId => provider.cast::<IScrollItemProvider>()?.cast(),
            _ => Err(Error::empty()),
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        self.key().read(|node, _| {
            Ok(match id {
                UIA_ControlTypePropertyId => control_type(node.role()).0.into(),
                UIA_NamePropertyId => BSTR::from(node.label().unwrap_or_default()).into(),
                UIA_HelpTextPropertyId => BSTR::from(node.description().unwrap_or_default()).into(),
                UIA_FrameworkIdPropertyId => BSTR::from("aterm").into(),
                UIA_IsEnabledPropertyId => (!node.is_disabled()).into(),
                UIA_IsOffscreenPropertyId => node.is_offscreen().into(),
                UIA_HasKeyboardFocusPropertyId => node.is_focused().into(),
                UIA_IsKeyboardFocusablePropertyId => {
                    (node.is_focused() || node.data().supports_action(Action::Focus)).into()
                }
                UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => {
                    (node.role() != Role::TextRun).into()
                }
                UIA_IsPasswordPropertyId => false.into(),
                UIA_LiveSettingPropertyId => match node.live() {
                    Live::Off => 0,
                    Live::Polite => 1,
                    Live::Assertive => 2,
                }
                .into(),
                _ => VARIANT::default(),
            })
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        let root = self.key().read(|node, _| Ok(node.is_root()))?;
        if !root {
            return Err(Error::empty());
        }
        let shared = self.shared.upgrade().ok_or_else(unavailable)?;
        unsafe { UiaHostProviderFromHwnd(HWND(shared.hwnd as *mut _)) }
    }
}

impl IRawElementProviderFragment_Impl for Provider_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
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
        key.ok_or_else(Error::empty)?.provider()?.cast()
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let id = self
            .key()
            .read(|node, _| Ok((!node.is_root()).then(|| runtime_id(node))))?;
        id.map_or(Ok(std::ptr::null_mut()), |id| array_i32(&id))
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        self.key().read(|node, state| {
            Ok(to_uia_rect(
                node.bounding_box().unwrap_or_default(),
                state.origin,
            ))
        })
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        self.key().dispatch(Action::Focus, None)
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.key().read(|_, _| Ok(()))?;
        self.shared
            .upgrade()
            .ok_or_else(unavailable)?
            .root()?
            .cast()
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

impl IRawElementProviderFragmentRoot_Impl for Provider_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        if !x.is_finite() || !y.is_finite() {
            return Err(E_INVALIDARG.into());
        }
        let key = self.key().read(|node, state| {
            let id = node
                .hit_test(
                    Point::new(x - state.origin.0, y - state.origin.1),
                    &common_filter,
                )
                .map(|(node, _)| node.id())
                .ok_or_else(Error::empty)?;
            Key::capture(&self.shared, id, state)
        })?;
        key.provider()?.cast()
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let key = self.key().read(|_, state| {
            state
                .publication
                .tree()
                .and_then(|tree| tree.focus_id())
                .map(|id| Key::capture(&self.shared, id, state))
                .transpose()
        })?;
        key.ok_or_else(Error::empty)?.provider()?.cast()
    }
}

impl IInvokeProvider_Impl for Provider_Impl {
    fn Invoke(&self) -> Result<()> {
        self.key().dispatch(Action::Click, None)
    }
}
impl IToggleProvider_Impl for Provider_Impl {
    fn Toggle(&self) -> Result<()> {
        self.key().dispatch(Action::Click, None)
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        self.key().read(|node, _| {
            Ok(match node.toggled().ok_or_else(unsupported)? {
                Toggled::True => ToggleState_On,
                Toggled::False => ToggleState_Off,
                Toggled::Mixed => ToggleState_Indeterminate,
            })
        })
    }
}
impl IValueProvider_Impl for Provider_Impl {
    fn SetValue(&self, value: &PCWSTR) -> Result<()> {
        if value.is_null() {
            return Err(E_INVALIDARG.into());
        }
        self.key().dispatch(
            Action::SetValue,
            Some(ActionData::Value(
                unsafe { value.to_string()? }.into_boxed_str(),
            )),
        )
    }
    fn Value(&self) -> Result<BSTR> {
        self.key()
            .read(|node, _| Ok(BSTR::from(crate::platform::text(node).unwrap_or_default())))
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        self.key()
            .read(|node, _| Ok((!node.data().supports_action(Action::SetValue)).into()))
    }
}
impl IRangeValueProvider_Impl for Provider_Impl {
    fn SetValue(&self, value: f64) -> Result<()> {
        if !value.is_finite() {
            return Err(E_INVALIDARG.into());
        }
        self.key().read(|node, _| {
            if node.min_numeric_value().is_some_and(|min| value < min)
                || node.max_numeric_value().is_some_and(|max| value > max)
            {
                return Err(E_INVALIDARG.into());
            }
            Ok(())
        })?;
        self.key()
            .dispatch(Action::SetValue, Some(ActionData::NumericValue(value)))
    }
    fn Value(&self) -> Result<f64> {
        self.key()
            .read(|node, _| node.numeric_value().ok_or_else(unsupported))
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        IValueProvider_Impl::IsReadOnly(self)
    }
    fn Maximum(&self) -> Result<f64> {
        self.key()
            .read(|node, _| node.max_numeric_value().ok_or_else(unsupported))
    }
    fn Minimum(&self) -> Result<f64> {
        self.key()
            .read(|node, _| node.min_numeric_value().ok_or_else(unsupported))
    }
    fn LargeChange(&self) -> Result<f64> {
        self.SmallChange()
    }
    fn SmallChange(&self) -> Result<f64> {
        self.key()
            .read(|node, _| Ok(node.numeric_value_step().unwrap_or(0.0)))
    }
}
impl IScrollItemProvider_Impl for Provider_Impl {
    fn ScrollIntoView(&self) -> Result<()> {
        self.key().dispatch(Action::ScrollIntoView, None)
    }
}
impl ISelectionItemProvider_Impl for Provider_Impl {
    fn Select(&self) -> Result<()> {
        self.key().dispatch(Action::Click, None)
    }
    fn AddToSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn IsSelected(&self) -> Result<BOOL> {
        self.key()
            .read(|node, _| Ok(node.is_selected().ok_or_else(unsupported)?.into()))
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        let key = self.key().read(|node, state| {
            node.selection_container(&common_filter)
                .map(|parent| Key::capture(&self.shared, parent.id(), state))
                .transpose()
        })?;
        key.ok_or_else(Error::empty)?.provider()
    }
}
impl ISelectionProvider_Impl for Provider_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let keys = self.key().read(|node, state| {
            node.filtered_children(&common_filter)
                .filter(|n| n.is_selected() == Some(true))
                .map(|n| Key::capture(&self.shared, n.id(), state))
                .collect::<Result<Vec<_>>>()
        })?;
        let values = keys
            .into_iter()
            .map(|key| key.provider()?.cast())
            .collect::<Result<Vec<IUnknown>>>()?;
        array_com(&values)
    }
    fn CanSelectMultiple(&self) -> Result<BOOL> {
        Ok(false.into())
    }
    fn IsSelectionRequired(&self) -> Result<BOOL> {
        Ok(false.into())
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

fn array(
    vt: VARENUM,
    len: usize,
    mut put: impl FnMut(*mut SAFEARRAY, i32) -> Result<()>,
) -> Result<*mut SAFEARRAY> {
    let count = i32::try_from(len).map_err(|_| Error::from(E_OUTOFMEMORY))?;
    let array = unsafe { SafeArrayCreateVector(vt, 0, count as u32) };
    if array.is_null() {
        return Err(E_OUTOFMEMORY.into());
    }
    for index in 0..count {
        if let Err(error) = put(array, index) {
            unsafe {
                let _ = SafeArrayDestroy(array);
            }
            return Err(error);
        }
    }
    Ok(array)
}
fn array_i32(values: &[i32]) -> Result<*mut SAFEARRAY> {
    array(VT_I4, values.len(), |array, index| unsafe {
        SafeArrayPutElement(
            array,
            &index,
            (&values[index as usize] as *const i32).cast(),
        )
    })
}
fn array_f64(values: &[f64]) -> Result<*mut SAFEARRAY> {
    array(VT_R8, values.len(), |array, index| unsafe {
        SafeArrayPutElement(
            array,
            &index,
            (&values[index as usize] as *const f64).cast(),
        )
    })
}
fn array_com(values: &[IUnknown]) -> Result<*mut SAFEARRAY> {
    array(VT_UNKNOWN, values.len(), |array, index| unsafe {
        SafeArrayPutElement(array, &index, values[index as usize].as_raw())
    })
}

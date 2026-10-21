// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NSAccessibility publishing for one real macOS content view. The same validated
//! tree and text consumer as AT-SPI supplies roles, children, geometry and text.
//! No application/window is created here. Construction requires a live NSView;
//! an absent view is inert, and the first assistive read requests the full tree.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use aterm_objc::{
    Bool, CGPoint, CGRect, CGSize, Id, MainThread, NSRange, Obj, Retained, Sel, WeakObj,
    checked_msg, class, ns_string, ns_string_to_rust, sel,
};

use crate::model::{
    Action, ActionData, ActionHandler, ActivationHandler, Live, Role, ScrollUnit, Toggled,
    TreeUpdate,
};
use crate::platform::{self as state, Changes, Handle, Publication};
use crate::tree::{FilterResult, Node, NodeId, common_filter};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    fn NSAccessibilityFrameInView(view: Id, frame: CGRect) -> CGRect;
    fn NSAccessibilityPostNotification(element: Id, notification: Id);
    fn NSAccessibilityPostNotificationWithUserInfo(element: Id, notification: Id, info: Id);
    static NSAccessibilityAnnouncementRequestedNotification: Id;
    static NSAccessibilityAnnouncementKey: Id;
    static NSAccessibilityPriorityKey: Id;
    static NSAccessibilityLayoutChangedNotification: Id;
    static NSAccessibilityValueChangedNotification: Id;
    static NSAccessibilityFocusedUIElementChangedNotification: Id;
}

// Strong references are created and used only on the main thread. Exported
// elements retain atomic weak counters, which may be dropped by a foreign
// final Objective-C release without dropping or accessing Context there.
type Shared = Arc<RefCell<Context>>;

struct Context {
    publication: Publication,
    view: WeakObj,
    host: Option<WeakObj>,
    elements: HashMap<NodeId, Retained<Element>>,
    activation: Rc<RefCell<Box<dyn ActivationHandler>>>,
    actions: Rc<RefCell<Box<dyn ActionHandler>>>,
    active: bool,
    closed: bool,
    scale: f64,
}

impl Context {
    /// Every semantic getter shares the held object's lifetime admission,
    /// including getters that only need the current focus, hit or window.
    fn read_live<R>(
        &self,
        handle: Option<&Handle>,
        read: impl FnOnce(&Context, Option<&Handle>) -> R,
    ) -> Option<R> {
        if self.closed {
            return None;
        }
        if let Some(handle) = handle {
            self.publication.node(handle)?;
        }
        Some(read(self, handle))
    }
}

struct ElementIvars {
    context: Weak<RefCell<Context>>,
    /// None is the lazy host group; other elements name one model insertion.
    handle: Option<Handle>,
}

aterm_objc::declare_class! {
    /// A retained semantic element, with all action ingress checked against its
    /// live model insertion before being posted to the host's event loop.
    struct Element: NSAccessibilityElement {
        const NAME: &str = "ATermSemanticAccessibilityElement";
        type Ivars = ElementIvars;

        @sel(isAccessibilityElement)
        fn is_element(&self) -> Bool {
            self.read(|cx, handle| match handle {
                None => true,
                Some(handle) => cx.publication.node(handle)
                    .is_some_and(|node| common_filter(&node) == FilterResult::Include),
            }).unwrap_or(false).into()
        }

        @sel(accessibilityRole)
        fn role(&self) -> Id {
            self.read(|cx, handle| match handle {
                None => string("AXGroup"),
                Some(handle) => cx.publication.node(handle)
                    .map_or(Id::NIL, |node| string(role(node.role()))),
            }).unwrap_or(Id::NIL)
        }

        @sel(accessibilityLabel)
        fn label(&self) -> Id {
            self.read(|cx, handle| match handle {
                None => string("aterm"),
                Some(handle) => cx.publication.node(handle).and_then(|node| {
                    if node.label_comes_from_value() { node.value() } else { node.label() }
                }).map_or(Id::NIL, |label| string(&label)),
            }).unwrap_or(Id::NIL)
        }

        @sel(accessibilityHelp)
        fn help(&self) -> Id {
            self.node_read(|node| node.description().map_or(Id::NIL, |s| string(&s)))
                .unwrap_or(Id::NIL)
        }

        @sel(accessibilityValue)
        fn value(&self) -> Id {
            self.node_read(|node| {
                if let Some(number) = node.numeric_value() {
                    number_object(number)
                } else if let Some(toggled) = node.toggled() {
                    number_object(match toggled { Toggled::False => 0., Toggled::True => 1., Toggled::Mixed => 2. })
                } else {
                    state::text(node).map_or(Id::NIL, |value| string(&value))
                }
            }).unwrap_or(Id::NIL)
        }

        @sel(accessibilityMinValue)
        fn min_value(&self) -> Id {
            self.node_read(|node| node.min_numeric_value().map_or(Id::NIL, number_object))
                .unwrap_or(Id::NIL)
        }

        @sel(accessibilityMaxValue)
        fn max_value(&self) -> Id {
            self.node_read(|node| node.max_numeric_value().map_or(Id::NIL, number_object))
                .unwrap_or(Id::NIL)
        }

        @sel(isAccessibilityEnabled)
        fn enabled(&self) -> Bool {
            self.read(|cx, handle| match handle {
                None => true,
                Some(handle) => cx.publication.node(handle).is_some_and(|node| !node.is_disabled()),
            }).unwrap_or(false).into()
        }

        @sel(isAccessibilitySelected)
        fn selected(&self) -> Bool {
            self.node_read(|node| node.data().is_selected().unwrap_or(false))
                .unwrap_or(false).into()
        }

        @sel(isAccessibilityExpanded)
        fn expanded(&self) -> Bool {
            self.node_read(|node| node.data().is_expanded().unwrap_or(false))
                .unwrap_or(false).into()
        }

        @sel(isAccessibilityFocused)
        fn focused(&self) -> Bool {
            self.node_read(|node| node.is_focused()).unwrap_or(false).into()
        }

        @sel(accessibilityParent)
        fn parent(&self) -> Id {
            self.read(|cx, handle| {
                let Some(handle) = handle else {
                    return cx.view.load().map_or(Id::NIL, Obj::autorelease);
                };
                let Some(node) = cx.publication.node(handle) else { return Id::NIL; };
                match node.filtered_parent(&common_filter) {
                    Some(parent) => element(cx, parent.id()),
                    None => cx.host.as_ref().and_then(WeakObj::load)
                        .map_or(Id::NIL, Obj::autorelease),
                }
            }).unwrap_or(Id::NIL)
        }

        @sel(accessibilityChildren)
        fn children(&self) -> Id {
            self.read(|cx, handle| {
                let Some(tree) = cx.publication.tree() else { return array([]); };
                let children = match handle {
                    None => vec![element(cx, tree.root_id())],
                    Some(handle) => {
                        let Some(node) = cx.publication.node(handle) else { return array([]); };
                        node.filtered_children(common_filter).map(|node| element(cx, node.id())).collect()
                    }
                };
                array(children)
            }).unwrap_or(Id::NIL)
        }

        @sel(accessibilitySelectedChildren)
        fn selected_children(&self) -> Id {
            self.read(|cx, handle| {
                let Some(node) = handle.and_then(|h| cx.publication.node(h)) else { return array([]); };
                array(node.filtered_children(common_filter)
                    .filter(|node| node.data().is_selected() == Some(true))
                    .map(|node| element(cx, node.id())))
            }).unwrap_or(Id::NIL)
        }

        @sel(accessibilityWindow)
        fn window(&self) -> Id {
            self.read(|cx, _| cx.view.load().map_or(Id::NIL, |view| {
                // SAFETY: the weak load retains a live NSView for this checked getter.
                unsafe {
                    let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                    send(view.id(), sel!(window))
                }
            })).unwrap_or(Id::NIL)
        }

        @sel(accessibilityTopLevelUIElement)
        fn top_level(&self) -> Id { self.window() }

        @sel(accessibilityFrame)
        fn frame(&self) -> CGRect {
            self.read(frame).unwrap_or_default()
        }

        @sel(accessibilityFocusedUIElement)
        fn focused_element(&self) -> Id {
            self.read(|cx, _| cx.publication.tree().and_then(|tree| tree.focus_id())
                .map_or(Id::NIL, |id| element(cx, id))).unwrap_or(Id::NIL)
        }

        @sel(accessibilityHitTest:)
        fn hit_test(&self, point: CGPoint) -> Id {
            self.read(|cx, _| {
                let Some(tree) = cx.publication.tree() else { return Id::NIL; };
                let bounds = frame(cx, None);
                let local = crate::model::Point::new(
                    (point.x - bounds.origin.x) * cx.scale,
                    (bounds.origin.y + bounds.size.height - point.y) * cx.scale,
                );
                tree.root().node_at_point(local, &common_filter)
                    .map_or(Id::NIL, |node| element(cx, node.id()))
            }).unwrap_or(Id::NIL)
        }

        @sel(accessibilityNumberOfCharacters)
        fn character_count(&self) -> isize {
            self.node_read(|node| state::text(node).map_or(0, |text|
                isize::try_from(text.encode_utf16().count()).unwrap_or(isize::MAX)))
                .unwrap_or(0)
        }

        @sel(accessibilitySelectedTextRange)
        fn selected_range(&self) -> NSRange {
            self.node_read(|node| state::selection(node).map_or(not_found(), |(location, length)|
                NSRange { location, length })).unwrap_or(not_found())
        }

        @sel(accessibilitySelectedText)
        fn selected_text(&self) -> Id {
            self.node_read(|node| {
                let (location, length) = state::selection(node)?;
                let text = state::text(node)?;
                Some(utf16_slice(&text, NSRange { location, length })
                    .map_or(Id::NIL, |value| string(&value)))
            }).flatten().unwrap_or(Id::NIL)
        }

        @sel(accessibilityStringForRange:)
        fn string_for_range(&self, range: NSRange) -> Id {
            self.node_read(|node| state::text(node).and_then(|value|
                utf16_slice(&value, range)).map_or(Id::NIL, |value| string(&value)))
                .unwrap_or(Id::NIL)
        }

        @sel(accessibilityLineForIndex:)
        fn line_for_index(&self, index: isize) -> isize {
            self.node_read(|node| state::text(node).and_then(|value| {
                let index = usize::try_from(index).ok()?;
                let prefix = utf16_slice(&value, NSRange { location: 0, length: index })?;
                isize::try_from(prefix.chars().filter(|ch| *ch == '\n').count()).ok()
            }).unwrap_or(-1)).unwrap_or(-1)
        }

        @sel(accessibilityRangeForLine:)
        fn range_for_line(&self, line: isize) -> NSRange {
            self.node_read(|node| state::text(node).map_or(not_found(), |value|
                line_range(&value, line))).unwrap_or(not_found())
        }

        @sel(accessibilityFrameForRange:)
        fn frame_for_range(&self, range: NSRange) -> CGRect {
            self.read(|cx, handle| {
                let Some(node) = handle.and_then(|handle| cx.publication.node(handle)) else { return CGRect::default(); };
                let Some(range) = state::text_range(node, range.location, range.length) else { return CGRect::default(); };
                let Some(bounds) = range.bounding_boxes().into_iter().reduce(|a, b| a.union(b)) else { return CGRect::default(); };
                screen_frame(cx, Some(bounds))
            }).unwrap_or_default()
        }

        @sel(accessibilityCustomActions)
        fn custom_actions(&self) -> Id {
            self.node_read(|node| {
                let mut actions = Vec::new();
                for (action, name, selector) in [
                    (Action::ScrollIntoView, "Show item", sel!(atermReveal)),
                    (Action::ScrollUp, "Scroll up", sel!(atermScrollUp)),
                    (Action::ScrollDown, "Scroll down", sel!(atermScrollDown)),
                ] {
                    if !node.is_disabled() && node.data().supports_action(action) {
                        actions.push(custom_action(self.as_id(), name, selector));
                    }
                }
                array(actions)
            }).unwrap_or(Id::NIL)
        }

        @sel(atermReveal)
        fn reveal(&self) -> Bool { self.request(Action::ScrollIntoView, None).into() }

        @sel(atermScrollUp)
        fn scroll_up(&self) -> Bool {
            self.request(Action::ScrollUp, Some(ActionData::ScrollUnit(ScrollUnit::Page))).into()
        }

        @sel(atermScrollDown)
        fn scroll_down(&self) -> Bool {
            self.request(Action::ScrollDown, Some(ActionData::ScrollUnit(ScrollUnit::Page))).into()
        }

        @sel(isAccessibilitySelectorAllowed:)
        fn selector_allowed(&self, selector: Sel) -> Bool {
            let action = if selector == sel!(accessibilityPerformPress) { Some(Action::Click) }
                else if selector == sel!(accessibilityPerformIncrement) { Some(Action::Increment) }
                else if selector == sel!(accessibilityPerformDecrement) { Some(Action::Decrement) }
                else if selector == sel!(accessibilityPerformShowMenu) { Some(Action::ShowContextMenu) }
                else if selector == sel!(setAccessibilityFocused:) { Some(Action::Focus) }
                else if selector == sel!(setAccessibilityValue:) { Some(Action::SetValue) }
                else if selector == sel!(setAccessibilitySelectedTextRange:) { Some(Action::SetTextSelection) }
                else if selector == sel!(setAccessibilitySelectedText:) { Some(Action::ReplaceSelectedText) }
                else if selector == sel!(atermReveal) { Some(Action::ScrollIntoView) }
                else if selector == sel!(atermScrollUp) { Some(Action::ScrollUp) }
                else if selector == sel!(atermScrollDown) { Some(Action::ScrollDown) }
                else { None };
            match action {
                Some(action) => self.node_read(|node| !node.is_disabled() && node.data().supports_action(action))
                    .unwrap_or(false).into(),
                None => {
                    let name = selector.name().to_bytes();
                    if name.starts_with(b"set") || name.starts_with(b"accessibilitySet") || name.starts_with(b"accessibilityPerform") || name.starts_with(b"aterm") {
                        return Bool::NO;
                    }
                    let live = self.read(|cx, handle| handle.is_none() ||
                        handle.and_then(|handle| cx.publication.node(handle)).is_some()).unwrap_or(false);
                    if !live { return Bool::NO; }
                    // SAFETY: ask the inherited NSAccessibilityElement policy for
                    // remaining read selectors. The exact prototype is -(BOOL)(SEL).
                    unsafe {
                        let send: unsafe extern "C-unwind" fn(*mut aterm_objc::ObjcSuper, Sel, Sel) -> Bool = aterm_objc::msg_super();
                        send(&mut self.super_receiver(), sel!(isAccessibilitySelectorAllowed:), selector)
                    }
                },
            }
        }

        @sel(accessibilityPerformPress)
        fn press(&self) -> Bool { self.request(Action::Click, None).into() }

        @sel(accessibilityPerformIncrement)
        fn increment(&self) -> Bool { self.request(Action::Increment, None).into() }

        @sel(accessibilityPerformDecrement)
        fn decrement(&self) -> Bool { self.request(Action::Decrement, None).into() }

        @sel(accessibilityPerformShowMenu)
        fn show_menu(&self) -> Bool { self.request(Action::ShowContextMenu, None).into() }

        @sel(setAccessibilityFocused:)
        fn set_focused(&self, focused: Bool) {
            self.request(if focused.as_bool() { Action::Focus } else { Action::Blur }, None);
        }

        @sel(setAccessibilityValue:)
        fn set_value(&self, value: Id) {
            let numeric = self.node_read(|node| node.numeric_value().is_some()).unwrap_or(false);
            let data = if numeric && is_class(value, c"NSNumber") {
                // SAFETY: the class check established NSNumber; doubleValue is -(double).
                let value = unsafe {
                    let send: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
                    send(value, sel!(doubleValue))
                };
                value.is_finite().then_some(ActionData::NumericValue(value))
            } else if !numeric && is_class(value, c"NSString") {
                // SAFETY: the class check established a live NSString.
                Some(ActionData::Value(unsafe { ns_string_to_rust(value) }.into_boxed_str()))
            } else { None };
            if let Some(data) = data { self.request(Action::SetValue, Some(data)); }
        }

        @sel(setAccessibilitySelectedText:)
        fn set_selected_text(&self, value: Id) {
            if is_class(value, c"NSString") {
                // SAFETY: the class check established NSString; conversion copies it.
                let value = unsafe { ns_string_to_rust(value) };
                self.request(Action::ReplaceSelectedText, Some(ActionData::Value(value.into_boxed_str())));
            }
        }

        @sel(setAccessibilitySelectedTextRange:)
        fn set_selected_range(&self, range: NSRange) {
            let selection = self.node_read(|node| state::text_selection(node, range.location, range.length)).flatten();
            if let Some(selection) = selection {
                self.request(Action::SetTextSelection, Some(ActionData::SetTextSelection(selection)));
            }
        }
    }
}

impl Element {
    fn read<R>(&self, read: impl FnOnce(&Context, Option<&Handle>) -> R) -> Option<R> {
        // AppKit normally queries on the main thread. Refuse an unexpected foreign
        // thread before reading any main-thread Rust ivars or ObjC object state.
        MainThread::new()?;
        let cx = self.ivars().context.upgrade()?;
        activate(&cx);
        let borrowed = cx.try_borrow().ok()?;
        borrowed.read_live(self.ivars().handle.as_ref(), read)
    }

    fn node_read<R>(&self, read: impl FnOnce(Node<'_>) -> R) -> Option<R> {
        self.read(|cx, handle| {
            handle
                .and_then(|handle| cx.publication.node(handle))
                .map(read)
        })?
    }

    fn request(&self, action: Action, data: Option<ActionData>) -> bool {
        let Some((request, handler)) = self
            .read(|cx, handle| {
                let request = cx.publication.action(handle?, action, data)?;
                Some((request, cx.actions.clone()))
            })
            .flatten()
        else {
            return false;
        };
        // Do not hold the tree's RefCell across a host callback.
        handler.borrow_mut().do_action(request);
        true
    }
}

/// One window's publisher. It never initializes NSApplication, registers a global
/// observer or reads the system's accessibility preferences.
pub struct Adapter {
    context: Shared,
    host: Retained<Element>,
}

impl Adapter {
    /// Attach to a real NSView. `None` refuses without touching AppKit, which is
    /// also the offscreen/test arm. A non-main-thread caller is refused.
    ///
    /// # Safety
    /// A supplied non-null `view` must be a live NSView on this thread. The
    /// publisher keeps a zeroing weak reference, so it cannot extend its life.
    pub unsafe fn new(
        view: Option<Id>,
        scale: f64,
        activation: impl ActivationHandler + 'static,
        actions: impl ActionHandler + 'static,
    ) -> Option<Self> {
        let view = view.filter(|view| !view.is_null())?;
        let mtm = MainThread::new()?;
        // SAFETY: caller supplies a live view, held by its window during this call.
        let view = unsafe { WeakObj::new(view) };
        #[expect(
            clippy::arc_with_non_send_sync,
            reason = "atomic exported weak drops; strong Context access remains main-thread-only"
        )]
        let context = Arc::new(RefCell::new(Context {
            publication: Publication::default(),
            view,
            host: None,
            elements: HashMap::new(),
            activation: Rc::new(RefCell::new(Box::new(activation))),
            actions: Rc::new(RefCell::new(Box::new(actions))),
            active: false,
            closed: false,
            scale: valid_scale(scale),
        }));
        let host = Element::alloc_init(
            mtm,
            ElementIvars {
                context: Arc::downgrade(&context),
                handle: None,
            },
        )?;
        // SAFETY: host is retained and cannot be deallocating.
        context.borrow_mut().host = Some(unsafe { WeakObj::new(host.as_id()) });
        let view = context.borrow().view.load()?;
        // SAFETY: live main-thread NSView; the array retains its child. Setting
        // children does not replace the NSView or its keyboard/IME implementation.
        unsafe {
            let set: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            set(
                view.id(),
                sel!(setAccessibilityChildren:),
                array([host.as_id()]),
            );
            let set_element: unsafe extern "C-unwind" fn(Id, Sel, Bool) = checked_msg();
            set_element(view.id(), sel!(setAccessibilityElement:), Bool::NO);
        }
        Some(Self { context, host })
    }

    /// Build nothing until an assistive query has requested the initial tree.
    pub fn update_if_active(&mut self, update: impl FnOnce() -> TreeUpdate) {
        if self.context.borrow().active {
            apply(&self.context, update());
        }
    }

    pub fn set_scale_factor(&mut self, scale: f64) {
        self.context.borrow_mut().scale = valid_scale(scale);
        notify(self.host.as_id(), Notification::Layout);
    }

    pub fn update_window_focus_state(&mut self, focused: bool) {
        let changes = self.context.borrow_mut().publication.set_focused(focused);
        announce(&self.context, changes);
    }

    /// The model's bounds stay in client pixels; screen conversion reads the
    /// current view/window, so a move needs only an extent-change notification.
    pub fn window_moved(&mut self) {
        if self.context.borrow().active {
            notify(self.host.as_id(), Notification::Layout);
        }
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        // Close activation and invalidate model leases before touching AppKit. A
        // retained element queried during teardown cannot restart this publisher.
        self.context.borrow_mut().closed = true;
        self.context.borrow_mut().active = false;
        self.context.borrow_mut().publication = Publication::default();
        if let Some(view) = self.context.borrow().view.load() {
            // SAFETY: the publisher is main-thread-only and this load retains a
            // still-live NSView; nil removes this publisher's children.
            unsafe {
                let set: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                set(view.id(), sel!(setAccessibilityChildren:), Id::NIL);
            }
        }
    }
}

fn activate(context: &Shared) {
    let activation = {
        let Ok(mut cx) = context.try_borrow_mut() else {
            return;
        };
        if cx.active || cx.closed {
            return;
        }
        cx.active = true;
        cx.activation.clone()
    };
    if let Some(update) = activation.borrow_mut().request_initial_tree() {
        apply(context, update);
    }
}

fn apply(context: &Shared, update: TreeUpdate) {
    let Some(mtm) = MainThread::new() else {
        return;
    };
    let changes = {
        let mut cx = context.borrow_mut();
        let changes = cx.publication.update(update);
        for id in changes.removed.iter().chain(&changes.replaced) {
            cx.elements.remove(id);
        }
        for id in changes.added.iter().chain(&changes.replaced) {
            let handle = cx
                .publication
                .handle(*id)
                .expect("new node has an element lease");
            if let Some(element) = Element::alloc_init(
                mtm,
                ElementIvars {
                    context: Arc::downgrade(context),
                    handle: Some(handle),
                },
            ) {
                cx.elements.insert(*id, element);
            }
        }
        changes
    };
    announce(context, changes);
}

enum Notification {
    Layout,
    Value,
    Focus,
}

fn notify(element: Id, notification: Notification) {
    if element.is_null() {
        return;
    }
    // SAFETY: each caller holds the live main-thread element across this call;
    // the framework constants have static lifetime and the C signature is exact.
    unsafe {
        let name = match notification {
            Notification::Layout => NSAccessibilityLayoutChangedNotification,
            Notification::Value => NSAccessibilityValueChangedNotification,
            Notification::Focus => NSAccessibilityFocusedUIElementChangedNotification,
        };
        NSAccessibilityPostNotification(element, name);
    }
}

fn announce_live(context: &Shared, live: Live, value: &str) {
    let application = {
        let cx = context.borrow();
        let Some(view) = cx.view.load() else {
            return;
        };
        // SAFETY: getters on a retained live view and its window; the window's
        // accessibility parent is the existing application, never +sharedApplication.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let window = get(view.id(), sel!(window));
            if window.is_null() {
                return;
            }
            Obj::retain(get(window, sel!(accessibilityParent)))
        }
    };
    let Some(application) = application else {
        return;
    };
    let Some(value) = ns_string(value) else {
        return;
    };
    // SAFETY: static framework NSString keys and an existing application element;
    // the dictionary factory copies/retains each counted key and value for the call.
    unsafe {
        let keys = [NSAccessibilityAnnouncementKey, NSAccessibilityPriorityKey];
        let values = [
            value.id(),
            number_object(if live == Live::Assertive { 90. } else { 50. }),
        ];
        let dictionary: unsafe extern "C-unwind" fn(
            aterm_objc::ClassPtr,
            Sel,
            *const Id,
            *const Id,
            usize,
        ) -> Id = checked_msg();
        let info = dictionary(
            class(c"NSDictionary"),
            sel!(dictionaryWithObjects:forKeys:count:),
            values.as_ptr(),
            keys.as_ptr(),
            keys.len(),
        );
        NSAccessibilityPostNotificationWithUserInfo(
            application.id(),
            NSAccessibilityAnnouncementRequestedNotification,
            info,
        );
    }
}

fn announce(context: &Shared, changes: Changes) {
    let (host, updated, focus) = {
        let cx = context.borrow();
        let host = (!changes.added.is_empty()
            || !changes.removed.is_empty()
            || !changes.replaced.is_empty())
        .then(|| cx.host.as_ref().and_then(WeakObj::load))
        .flatten();
        let updated: Vec<_> = changes
            .updated
            .iter()
            .filter_map(|id| cx.elements.get(id).map(Retained::clone_retained))
            .collect();
        let focus = changes
            .focus
            .then(|| {
                cx.publication
                    .tree()
                    .and_then(|tree| tree.focus_id())
                    .and_then(|id| cx.elements.get(&id).map(Retained::clone_retained))
            })
            .flatten();
        (host, updated, focus)
    };
    // OS notification delivery can call back synchronously. No tree borrow lives
    // across these calls, and retained objects survive a reentrant update.
    if let Some(host) = host {
        notify(host.id(), Notification::Layout);
    }
    for element in updated {
        notify(element.as_id(), Notification::Value);
    }
    if let Some(focus) = focus {
        notify(focus.as_id(), Notification::Focus);
    }
    for (_, live, value) in changes.announcements {
        announce_live(context, live, &value);
    }
}

fn element(context: &Context, id: NodeId) -> Id {
    context
        .elements
        .get(&id)
        .map_or(Id::NIL, |element| element.clone_retained().autorelease())
}

fn string(value: &str) -> Id {
    ns_string(value).map_or(Id::NIL, Obj::autorelease)
}

fn array(items: impl IntoIterator<Item = Id>) -> Id {
    // SAFETY: NSMutableArray's +new returns +1; addObject: retains live, non-null
    // objects. The array is returned autoreleased, as an ObjC getter requires.
    unsafe {
        let new: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel) -> Id = checked_msg();
        let Some(array) = Obj::from_owned(new(class(c"NSMutableArray"), sel!(new))) else {
            return Id::NIL;
        };
        let add: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
        for item in items {
            if !item.is_null() {
                add(array.id(), sel!(addObject:), item);
            }
        }
        array.autorelease()
    }
}

fn number_object(value: f64) -> Id {
    // SAFETY: NSNumber +numberWithDouble: is +(id)(double), autoreleased.
    unsafe {
        let send: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel, f64) -> Id = checked_msg();
        send(class(c"NSNumber"), sel!(numberWithDouble:), value)
    }
}

fn is_class(value: Id, name: &'static std::ffi::CStr) -> bool {
    if value.is_null() || MainThread::new().is_none() {
        return false;
    }
    // SAFETY: an Objective-C setter supplies an object; NSObject's class query
    // establishes the more specific class before any class-specific getter.
    unsafe {
        let send: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::ClassPtr) -> Bool =
            checked_msg();
        send(value, sel!(isKindOfClass:), class(name)).as_bool()
    }
}

fn custom_action(target: Id, name: &str, selector: Sel) -> Id {
    let Some(name) = ns_string(name) else {
        return Id::NIL;
    };
    // SAFETY: the SDK's designated initializer consumes +alloc and takes a
    // live NSString, weak NSObject target and a BOOL-returning selector.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel) -> Id = checked_msg();
        let raw = alloc(class(c"NSAccessibilityCustomAction"), sel!(alloc));
        if raw.is_null() {
            return Id::NIL;
        }
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Sel) -> Id = checked_msg();
        Obj::from_owned(init(
            raw,
            sel!(initWithName:target:selector:),
            name.id(),
            target,
            selector,
        ))
        .map_or(Id::NIL, Obj::autorelease)
    }
}

fn valid_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0. {
        scale
    } else {
        1.
    }
}

fn frame(cx: &Context, handle: Option<&Handle>) -> CGRect {
    match handle {
        None => screen_frame(cx, None),
        Some(handle) => {
            let Some(node) = cx.publication.node(handle) else {
                return CGRect::default();
            };
            match node.bounding_box() {
                Some(bounds) => screen_frame(cx, Some(bounds)),
                None if node.is_root() => screen_frame(cx, None),
                None => CGRect::default(),
            }
        }
    }
}

fn screen_frame(cx: &Context, rect: Option<crate::model::Rect>) -> CGRect {
    let Some(view) = cx.view.load() else {
        return CGRect::default();
    };
    // SAFETY: this main-thread NSView is retained across its geometry getters;
    // NSAccessibilityFrameInView converts its local points to screen points.
    unsafe {
        let bounds: unsafe extern "C-unwind" fn(Id, Sel) -> CGRect = checked_msg();
        let bounds = bounds(view.id(), sel!(bounds));
        let rect = match rect {
            None => bounds,
            Some(rect) => {
                let flipped: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = checked_msg();
                CGRect {
                    origin: CGPoint {
                        x: bounds.origin.x + rect.x0 / cx.scale,
                        y: bounds.origin.y
                            + if flipped(view.id(), sel!(isFlipped)).as_bool() {
                                rect.y0 / cx.scale
                            } else {
                                bounds.size.height - rect.y1 / cx.scale
                            },
                    },
                    size: CGSize {
                        width: (rect.x1 - rect.x0).max(0.) / cx.scale,
                        height: (rect.y1 - rect.y0).max(0.) / cx.scale,
                    },
                }
            }
        };
        NSAccessibilityFrameInView(view.id(), rect)
    }
}

fn role(role: Role) -> &'static str {
    match role {
        Role::TextRun | Role::Label | Role::Heading => "AXStaticText",
        Role::Link => "AXLink",
        Role::MenuItem | Role::MenuItemRadio => "AXMenuItem",
        Role::TextInput | Role::SearchInput => "AXTextField",
        Role::Terminal | Role::MultilineTextInput | Role::Document => "AXTextArea",
        Role::Button => "AXButton",
        Role::Switch => "AXCheckBox",
        Role::List | Role::ListBox => "AXList",
        Role::ListBoxOption => "AXRow",
        Role::Menu => "AXMenu",
        Role::ProgressIndicator => "AXProgressIndicator",
        // A gauge, not a task: VoiceOver reads a level indicator's value as
        // a level ("3.1 of 10").
        Role::Meter => "AXLevelIndicator",
        Role::Grid => "AXGrid",
        Role::Slider => "AXSlider",
        Role::Tab => "AXRadioButton",
        Role::TabList => "AXTabGroup",
        Role::Alert
        | Role::AlertDialog
        | Role::Application
        | Role::Group
        | Role::Navigation
        | Role::Status
        | Role::Window
        | Role::Unknown => "AXGroup",
    }
}

fn not_found() -> NSRange {
    NSRange {
        location: isize::MAX as usize,
        length: 0,
    }
}

fn utf16_slice(value: &str, range: NSRange) -> Option<String> {
    let units: Vec<_> = value.encode_utf16().collect();
    String::from_utf16(units.get(range.location..range.location.checked_add(range.length)?)?).ok()
}

fn line_range(value: &str, line: isize) -> NSRange {
    let Ok(line) = usize::try_from(line) else {
        return not_found();
    };
    let mut location = 0;
    for (index, text) in value.split_inclusive('\n').enumerate() {
        let length = text.encode_utf16().count();
        if index == line {
            return NSRange { location, length };
        }
        location += length;
    }
    if (value.is_empty() && line == 0) || (value.ends_with('\n') && line == value.lines().count()) {
        NSRange {
            location,
            length: 0,
        }
    } else {
        not_found()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ActionRequest;

    struct Inert;
    impl ActivationHandler for Inert {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            panic!("offscreen activation");
        }
    }
    impl ActionHandler for Inert {
        fn do_action(&mut self, _: ActionRequest) {
            panic!("offscreen action");
        }
    }

    /// The stats bars' gauges read as level indicators and their core
    /// heatmap as a grid; a progress bar stays a progress indicator.
    #[test]
    fn a_meter_is_a_level_indicator_and_a_grid_a_grid() {
        assert_eq!(role(Role::Meter), "AXLevelIndicator");
        assert_eq!(role(Role::Grid), "AXGrid");
        assert_eq!(role(Role::ProgressIndicator), "AXProgressIndicator");
    }

    #[test]
    fn no_native_view_is_inert_without_initializing_appkit() {
        // SAFETY: no view is supplied, so no native precondition is required.
        assert!(unsafe { Adapter::new(None, 1., Inert, Inert) }.is_none());
    }

    /// Exercise the exact common getter gate without an AppKit object or view.
    /// The closures model focus/hit/window getters, which need not themselves
    /// inspect their element handle. A retired object must invoke none of them.
    #[test]
    fn retired_elements_cannot_read_replacement_focus_hits_or_window() {
        use crate::model::{
            ActionContext, Node as ModelNode, NodeId as ModelId, Point, Rect, Tree, TreeId,
        };
        let update = |generation, present| {
            let mut root = ModelNode::new(Role::Window);
            root.set_bounds(Rect::new(0., 0., 100., 100.));
            root.set_children(if present { vec![ModelId(1)] } else { vec![] });
            let mut nodes = vec![(ModelId(0), root)];
            if present {
                let mut button = ModelNode::new(Role::Button);
                button.set_bounds(Rect::new(0., 0., 40., 40.));
                button.set_action_context(ActionContext::new(11, generation));
                nodes.push((ModelId(1), button));
            }
            TreeUpdate {
                nodes,
                tree: Some(Tree::new(ModelId(0))),
                tree_id: TreeId::ROOT,
                focus: if present { ModelId(1) } else { ModelId(0) },
            }
        };
        let mut cx = Context {
            publication: Publication::default(),
            // SAFETY: nil initializes an empty weak runtime slot; no AppKit call.
            view: unsafe { WeakObj::new(Id::NIL) },
            host: None,
            elements: HashMap::new(),
            activation: Rc::new(RefCell::new(Box::new(Inert))),
            actions: Rc::new(RefCell::new(Box::new(Inert))),
            active: true,
            closed: false,
            scale: 1.,
        };
        cx.publication.update(update(7, true));
        cx.publication.set_focused(true);
        let old = cx.publication.handle(NodeId(ModelId(1))).unwrap();
        let focus = |cx: &Context, _: Option<&Handle>| cx.publication.tree()?.focus_id();
        let hit = |cx: &Context, _: Option<&Handle>| {
            cx.publication
                .tree()?
                .root()
                .node_at_point(Point::new(10., 10.), &common_filter)
                .map(|node| node.id())
        };
        assert_eq!(
            cx.read_live(Some(&old), focus),
            Some(Some(NodeId(ModelId(1))))
        );
        assert_eq!(
            cx.read_live(Some(&old), hit),
            Some(Some(NodeId(ModelId(1))))
        );
        cx.publication.update(update(8, true));
        assert!(cx.read_live(Some(&old), focus).is_none());
        assert!(cx.read_live(Some(&old), hit).is_none());
        let window_read = std::cell::Cell::new(false);
        assert!(
            cx.read_live(Some(&old), |_, _| window_read.set(true))
                .is_none()
        );
        assert!(
            !window_read.get(),
            "a retired element never reaches native window reads"
        );
        let fresh = cx.publication.handle(NodeId(ModelId(1))).unwrap();
        assert!(cx.read_live(Some(&fresh), focus).is_some());
        cx.publication.update(update(8, true));
        assert!(
            cx.read_live(Some(&fresh), hit).is_some(),
            "ordinary updates retain identity"
        );
        cx.publication.update(update(8, false));
        assert!(cx.read_live(Some(&fresh), focus).is_none());
        cx.publication.update(update(8, true));
        assert!(
            cx.read_live(Some(&fresh), hit).is_none(),
            "reinsertion is a new object"
        );
        assert!(
            cx.read_live(None, focus).is_some(),
            "the live lazy host may read"
        );
        cx.closed = true;
        assert!(
            cx.read_live(None, focus).is_none(),
            "a closed host may not read"
        );
    }

    #[test]
    fn text_ranges_use_utf16_and_reject_partial_surrogates() {
        let value = "a🦀\nz";
        assert_eq!(
            utf16_slice(
                value,
                NSRange {
                    location: 1,
                    length: 2
                }
            )
            .as_deref(),
            Some("🦀")
        );
        assert!(
            utf16_slice(
                value,
                NSRange {
                    location: 2,
                    length: 1
                }
            )
            .is_none()
        );
        assert!(
            utf16_slice(
                value,
                NSRange {
                    location: usize::MAX,
                    length: 1
                }
            )
            .is_none()
        );
        let first = line_range(value, 0);
        assert_eq!((first.location, first.length), (0, 4));
        let second = line_range(value, 1);
        assert_eq!((second.location, second.length), (4, 1));
        assert_eq!(line_range(value, 2).location, isize::MAX as usize);
    }
}

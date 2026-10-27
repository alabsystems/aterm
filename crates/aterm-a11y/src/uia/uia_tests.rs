// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows-only controls exercise the actual COM objects and method dispatch,
//! through their vtables as UI Automation calls them. They do not create an
//! HWND, inspect the desktop or claim Narrator acceptance.

#![cfg(test)]

use std::ptr;

use aterm_win32::Win32::Foundation::{E_NOINTERFACE, E_POINTER, SysFreeString};
use aterm_win32::Win32::System::Variant::{VARIANT, VariantClear};
use aterm_win32::Win32::UI::Accessibility::{
    SupportedTextSelection_None, SupportedTextSelection_Single, TextPatternRangeEndpoint_End,
    TextPatternRangeEndpoint_Start, TextUnit, TextUnit_Character, TextUnit_Word,
    UIA_IsReadOnlyAttributeId,
};
use aterm_win32::com::{IID_IAgileObject, IID_IInspectable, IID_IMarshal};
use aterm_win32::uia::{
    IID_IRawElementProviderSimple, IID_ITextProvider, IInvokeProvider_Vtbl,
    IRawElementProviderFragment_Vtbl, IRawElementProviderSimple_Vtbl, ISelectionItemProvider_Vtbl,
    ITextProvider_Vtbl, ITextProvider2_Vtbl, ITextRangeProvider_Vtbl,
};

use super::values::{Bstr, bstr_text};
use super::*;

struct Activation;
impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        None
    }
}
struct Actions(Arc<Mutex<Vec<model::ActionRequest>>>);
impl ActionHandler for Actions {
    fn do_action(&mut self, action: model::ActionRequest) {
        self.0.lock_or_recover().push(action);
    }
}

fn update(button: bool) -> TreeUpdate {
    let mut root = model::Node::new(Role::Terminal);
    root.set_read_only();
    root.set_label("terminal");
    let mut run = model::Node::new(Role::TextRun);
    run.set_value("A😀e\u{301}\n");
    run.set_character_lengths(vec![1, 4, 3, 1]);
    run.set_word_starts(vec![0, 2]);
    root.set_children(if button {
        vec![model::NodeId(2), model::NodeId(3)]
    } else {
        vec![model::NodeId(2)]
    });
    root.set_text_selection(model::TextSelection {
        anchor: model::TextPosition {
            node: model::NodeId(2),
            character_index: 1,
        },
        focus: model::TextPosition {
            node: model::NodeId(2),
            character_index: 1,
        },
    });
    let mut nodes = vec![(model::NodeId(1), root), (model::NodeId(2), run)];
    if button {
        let mut button = model::Node::new(Role::Button);
        button.set_label("Run");
        button.add_action(Action::Click);
        button.set_action_context(model::ActionContext::new(41, 7));
        nodes.push((model::NodeId(3), button));
    }
    TreeUpdate {
        nodes,
        tree: Some(model::Tree::new(model::NodeId(1))),
        tree_id: model::TreeId::ROOT,
        focus: model::NodeId(1),
    }
}

fn fixture() -> (Arc<Shared>, Arc<Mutex<Vec<model::ActionRequest>>>) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let mut publication = Publication::default();
    publication.update(update(true));
    let shared = Arc::new(Shared {
        hwnd: 0,
        state: Mutex::new(State {
            publication,
            origin: (0.0, 0.0),
        }),
        root: OnceLock::new(),
        active: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        activation: Mutex::new(Box::new(Activation)),
        actions: Mutex::new(Box::new(Actions(actions.clone()))),
    });
    let _ = shared.root.set(Agile::new(objects::provider(Provider {
        shared: Arc::downgrade(&shared),
        handle: None,
    })));
    (shared, actions)
}

/// `com`'s vtable as `V`.
fn vt<V>(com: &Com) -> &V {
    // SAFETY: each caller names the interface `com` was queried as.
    unsafe { com.vtable::<V>() }
}

fn ok(hr: HRESULT) -> Result<()> {
    if hr < 0 { Err(hr) } else { Ok(()) }
}

/// A slot's interface result, owned.
fn interface(f: impl FnOnce(*mut *mut c_void) -> HRESULT) -> Result<Option<Com>> {
    let mut out = ptr::null_mut();
    ok(f(&mut out))?;
    // SAFETY: a successful slot hands over the reference `out` holds.
    Ok(unsafe { Com::from_raw(out) })
}

/// A slot's plain result.
fn value<T>(init: T, f: impl FnOnce(*mut T) -> HRESULT) -> Result<T> {
    let mut out = init;
    ok(f(&mut out))?;
    Ok(out)
}

/// A slot's BSTR result, read and freed.
fn text(f: impl FnOnce(*mut *mut c_void) -> HRESULT) -> Result<String> {
    let raw = value(ptr::null_mut(), f)?.cast_const().cast::<u16>();
    // SAFETY: a successful slot hands over the BSTR, freed once here.
    let text = unsafe { bstr_text(raw) };
    // SAFETY: as above.
    unsafe { SysFreeString(raw) };
    Ok(text)
}

fn name(simple: &Com) -> Result<()> {
    let mut out = VARIANT::default();
    let v = vt::<IRawElementProviderSimple_Vtbl>(simple);
    // SAFETY: `simple` is an IRawElementProviderSimple; `out` is writable.
    ok(unsafe { (v.GetPropertyValue)(simple.as_raw(), UIA_NamePropertyId, &mut out) })?;
    // SAFETY: the VARIANT the slot handed over, cleared once.
    unsafe { VariantClear(&mut out) };
    Ok(())
}

fn invoke(invoke: &Com) -> Result<()> {
    // SAFETY: `invoke` is an IInvokeProvider.
    ok(unsafe { (vt::<IInvokeProvider_Vtbl>(invoke).Invoke)(invoke.as_raw()) })
}

fn document_range(text: &Com) -> Result<Com> {
    let v = vt::<ITextProvider_Vtbl>(text);
    // SAFETY: `text` is an ITextProvider (ITextProvider2 begins with its slots).
    interface(|out| unsafe { (v.DocumentRange)(text.as_raw(), out) })
        .map(|range| range.expect("a document range"))
}

fn range_vt(range: &Com) -> &ITextRangeProvider_Vtbl {
    vt::<ITextRangeProvider_Vtbl>(range)
}

fn get_text(range: &Com, max: i32) -> Result<String> {
    // SAFETY: `range` is an ITextRangeProvider.
    text(|out| unsafe { (range_vt(range).GetText)(range.as_raw(), max, out) })
}

fn move_by(range: &Com, unit: TextUnit, count: i32) -> Result<i32> {
    // SAFETY: as in `get_text`.
    value(0, |out| unsafe {
        (range_vt(range).Move)(range.as_raw(), unit, count, out)
    })
}

fn select(range: &Com) -> Result<()> {
    // SAFETY: as in `get_text`.
    ok(unsafe { (range_vt(range).Select)(range.as_raw()) })
}

fn enclosing(range: &Com) -> Result<Com> {
    // SAFETY: as in `get_text`.
    interface(|out| unsafe { (range_vt(range).GetEnclosingElement)(range.as_raw(), out) })
        .map(|element| element.expect("an enclosing element"))
}

fn supported(text: &Com) -> Result<i32> {
    let v = vt::<ITextProvider_Vtbl>(text);
    // SAFETY: as in `document_range`.
    value(0, |out| unsafe {
        (v.SupportedTextSelection)(text.as_raw(), out)
    })
}

#[test]
fn interface_queries_share_identity_and_owned_references_outlive_the_original() {
    let (shared, _) = fixture();
    let root = shared.root().unwrap();
    let identity = root.query(&IID_IUnknown).unwrap();
    let fragment = root.query(&IID_IRawElementProviderFragment).unwrap();
    let text = root.query(&IID_ITextProvider2).unwrap();
    for other in [&fragment, &text] {
        assert_eq!(
            identity.as_raw(),
            other.query(&IID_IUnknown).unwrap().as_raw()
        );
    }
    for iid in [IID_IInspectable, IID_IAgileObject] {
        assert_eq!(root.query(&iid).unwrap().as_raw(), identity.as_raw());
    }
    assert_eq!(
        root.query(&IID_ITextProvider).unwrap().as_raw(),
        text.as_raw(),
        "ITextProvider2's slot answers its base interface"
    );
    assert!(
        root.query(&IID_IMarshal).is_ok(),
        "the free-threaded marshaler aggregates"
    );
    assert_eq!(
        root.query(&aterm_win32::uia::IID_ITextRangeProvider).err(),
        Some(E_NOINTERFACE)
    );
    let held = root.query(&IID_IRawElementProviderSimple).unwrap();
    drop(root);
    drop(identity);
    drop(fragment);
    drop(text);
    assert!(name(&held).is_ok());
    let weak = Arc::downgrade(&shared);
    drop(shared);
    assert!(
        weak.upgrade().is_none(),
        "provider references do not retain a closed window's model"
    );
    assert_eq!(
        name(&held).err(),
        Some(ELEMENT_NOT_AVAILABLE),
        "a dropped publication is unavailable"
    );
}

#[test]
fn retained_child_does_not_act_after_removal_or_reinsertion() {
    let (shared, actions) = fixture();
    let button = shared.provider(NodeId(model::NodeId(3))).unwrap();
    let original_id = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .unwrap()
        .node_by_id(NodeId(model::NodeId(3)))
        .map(runtime_id)
        .unwrap();
    let held = button.query(&IID_IInvokeProvider).unwrap();
    invoke(&held).unwrap();
    assert_eq!(actions.lock_or_recover().len(), 1);
    assert_eq!(actions.lock_or_recover()[0].target_node, model::NodeId(3));
    assert_eq!(
        actions.lock_or_recover()[0].context,
        Some(model::ActionContext::new(41, 7))
    );
    shared
        .state
        .lock_or_recover()
        .publication
        .update(update(false));
    assert_eq!(invoke(&held), Err(ELEMENT_NOT_AVAILABLE));
    shared
        .state
        .lock_or_recover()
        .publication
        .update(update(true));
    let reinserted_id = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .unwrap()
        .node_by_id(NodeId(model::NodeId(3)))
        .map(runtime_id)
        .unwrap();
    assert_ne!(
        original_id, reinserted_id,
        "same id and context still name a new insertion"
    );
    assert_eq!(invoke(&held), Err(ELEMENT_NOT_AVAILABLE));
    let fresh = shared
        .provider(NodeId(model::NodeId(3)))
        .unwrap()
        .query(&IID_IInvokeProvider)
        .unwrap();
    invoke(&fresh).unwrap();
    assert_eq!(actions.lock_or_recover().len(), 2);
    let before = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .and_then(|tree| tree.node_by_id(NodeId(model::NodeId(3))))
        .map(runtime_id)
        .unwrap();
    let mut replacement = update(true);
    replacement
        .nodes
        .iter_mut()
        .find(|(id, _)| *id == model::NodeId(3))
        .unwrap()
        .1
        .set_action_context(model::ActionContext::new(41, 8));
    shared
        .state
        .lock_or_recover()
        .publication
        .update(replacement);
    assert_eq!(invoke(&fresh), Err(ELEMENT_NOT_AVAILABLE));
    let after = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .and_then(|tree| tree.node_by_id(NodeId(model::NodeId(3))))
        .map(runtime_id)
        .unwrap();
    assert_ne!(
        before, after,
        "UIA runtime identity carries the exact lifecycle too"
    );
    let current = shared
        .provider(NodeId(model::NodeId(3)))
        .unwrap()
        .query(&IID_IInvokeProvider)
        .unwrap();
    invoke(&current).unwrap();
    assert_eq!(
        actions.lock_or_recover()[2].context,
        Some(model::ActionContext::new(41, 8))
    );
    assert_eq!(
        actions.lock_or_recover()[0].context,
        Some(model::ActionContext::new(41, 7)),
        "queued receipt retains original context"
    );
}

#[test]
fn text_methods_navigate_model_characters_and_refuse_terminal_mutation() {
    let (shared, actions) = fixture();
    let text = shared.root().unwrap().query(&IID_ITextProvider2).unwrap();
    let range = document_range(&text).unwrap();
    assert_eq!(get_text(&range, -1).unwrap(), "A😀e\u{301}\n");
    assert_eq!(get_text(&range, 2).unwrap(), "A");
    assert_eq!(move_by(&range, TextUnit_Word, 0).unwrap(), 0);
    assert_eq!(
        get_text(&range, -1).unwrap(),
        "A😀e\u{301}\n",
        "zero movement must not normalize or shrink a nondegenerate range"
    );
    assert_eq!(supported(&text).unwrap(), SupportedTextSelection_None);
    assert_eq!(select(&range), Err(NOT_SUPPORTED));
    assert!(actions.lock_or_recover().is_empty());
    // SAFETY: `range` is an ITextRangeProvider.
    let clone = interface(|out| unsafe { (range_vt(&range).Clone)(range.as_raw(), out) })
        .unwrap()
        .unwrap();
    // SAFETY: as above, for `clone`.
    ok(unsafe { (range_vt(&clone).ExpandToEnclosingUnit)(clone.as_raw(), TextUnit_Character) })
        .unwrap();
    assert_eq!(get_text(&clone, -1).unwrap(), "A");
    assert_eq!(move_by(&clone, TextUnit_Character, 1).unwrap(), 1);
    assert_eq!(get_text(&clone, -1).unwrap(), "😀");
    assert_eq!(move_by(&clone, TextUnit_Character, 1).unwrap(), 1);
    assert_eq!(get_text(&clone, -1).unwrap(), "e\u{301}");
    assert_eq!(
        get_text(&range, -1).unwrap(),
        "A😀e\u{301}\n",
        "Clone has independent endpoints"
    );
    let needle = Bstr::new("😀").unwrap();
    // SAFETY: `range` is an ITextRangeProvider; `needle` stays ours.
    let found = interface(|out| unsafe {
        (range_vt(&range).FindText)(range.as_raw(), needle.as_raw().cast_mut().cast(), 0, 0, out)
    })
    .unwrap()
    .unwrap();
    // SAFETY: both are this window's ranges.
    assert!(
        value(0, |out| unsafe {
            (range_vt(&found).Compare)(found.as_raw(), clone.as_raw(), out)
        })
        .is_ok()
    );
    assert_eq!(get_text(&found, -1).unwrap(), "😀");
    let mut active: BOOL = 0;
    let caret_slot = vt::<ITextProvider2_Vtbl>(&text).GetCaretRange;
    // SAFETY: `text` is an ITextProvider2; `active` is writable.
    let caret = interface(|out| unsafe { caret_slot(text.as_raw(), &mut active, out) })
        .unwrap()
        .unwrap();
    assert_eq!(get_text(&caret, -1).unwrap(), "");
    assert_eq!(
        // SAFETY: both are this window's ranges.
        value(9, |out| unsafe {
            (range_vt(&caret).CompareEndpoints)(
                caret.as_raw(),
                TextPatternRangeEndpoint_Start,
                found.as_raw(),
                TextPatternRangeEndpoint_Start,
                out,
            )
        })
        .unwrap(),
        0
    );
    let mut editor = update(true);
    let root = &mut editor.nodes[0].1;
    root.set_role(Role::Document);
    root.set_action_context(model::ActionContext::new(41, 9));
    root.add_action(Action::SetTextSelection);
    shared.state.lock_or_recover().publication.update(editor);
    assert_eq!(
        select(&range),
        Err(ELEMENT_NOT_AVAILABLE),
        "a range pins even a document hosted at the fragment root"
    );
    assert_eq!(enclosing(&range).err(), Some(ELEMENT_NOT_AVAILABLE));
    let simple = shared.root().unwrap();
    let pattern_slot = vt::<IRawElementProviderSimple_Vtbl>(&simple).GetPatternProvider;
    // SAFETY: `simple` is an IRawElementProviderSimple.
    let pattern =
        interface(|out| unsafe { pattern_slot(simple.as_raw(), UIA_TextPattern2Id, out) })
            .unwrap()
            .unwrap()
            .query(&IID_ITextProvider2)
            .unwrap();
    let fresh = document_range(&pattern).unwrap();
    assert_eq!(supported(&text).unwrap(), SupportedTextSelection_Single);
    select(&fresh).unwrap();
    assert_eq!(
        // SAFETY: `fresh` is an ITextRangeProvider.
        ok(unsafe { (range_vt(&fresh).AddToSelection)(fresh.as_raw()) }),
        Err(NOT_SUPPORTED)
    );
    assert_eq!(
        actions.lock_or_recover().len(),
        1,
        "single-selection addition must not replace the selection"
    );
    let selected = enclosing(&fresh)
        .unwrap()
        .query(&IID_ITextProvider2)
        .unwrap();
    assert_eq!(supported(&selected).unwrap(), SupportedTextSelection_Single);
    let action = actions.lock_or_recover()[0].clone();
    assert_eq!(action.action, Action::SetTextSelection);
    assert_eq!(action.context, Some(model::ActionContext::new(41, 9)));
    let Some(ActionData::SetTextSelection(selection)) = action.data else {
        panic!("selection data missing");
    };
    assert_eq!(selection.anchor.character_index, 0);
    assert_eq!(selection.focus.character_index, 4);
    let mut replacement = update(true);
    replacement.nodes[0]
        .1
        .set_action_context(model::ActionContext::new(41, 10));
    replacement.nodes[0].1.add_action(Action::SetTextSelection);
    shared
        .state
        .lock_or_recover()
        .publication
        .update(replacement);
    assert_eq!(
        document_range(&pattern).err(),
        Some(ELEMENT_NOT_AVAILABLE),
        "a held pattern cannot retarget a replacement document"
    );
    shared.closed.store(true, Ordering::Release);
    assert_eq!(get_text(&fresh, -1), Err(ELEMENT_NOT_AVAILABLE));
}

#[test]
fn text_ranges_from_another_window_cannot_supply_endpoints() {
    let (a, _) = fixture();
    let (b, _) = fixture();
    let a = a.root().unwrap().query(&IID_ITextProvider).unwrap();
    let b = b.root().unwrap().query(&IID_ITextProvider).unwrap();
    let one = document_range(&a).unwrap();
    let other = document_range(&b).unwrap();
    // SAFETY: both are ITextRangeProviders.
    let moved = unsafe {
        (range_vt(&one).MoveEndpointByRange)(
            one.as_raw(),
            TextPatternRangeEndpoint_Start,
            other.as_raw(),
            TextPatternRangeEndpoint_End,
        )
    };
    assert_eq!(moved, E_INVALIDARG);
    // SAFETY: as above; a range is no element.
    let child = unsafe {
        (vt::<ITextProvider_Vtbl>(&a).RangeFromChild)(
            a.as_raw(),
            one.as_raw(),
            &mut ptr::null_mut(),
        )
    };
    assert_eq!(
        child, E_INVALIDARG,
        "only this window's elements are children"
    );
}

#[test]
fn absent_hwnd_and_other_object_ids_are_inert() {
    assert!(
        unsafe { Adapter::new(0, Activation, Actions(Arc::new(Mutex::new(Vec::new())))) }.is_none()
    );
    assert_eq!(handle_wm_getobject(0, 0, UiaRootObjectId as isize), None);
    assert_eq!(handle_wm_getobject(0, 0, -4), None);
}

#[test]
fn a_searched_attribute_is_read_through_the_callers_conversion() {
    let (shared, _) = fixture();
    let text = shared.root().unwrap().query(&IID_ITextProvider2).unwrap();
    let range = document_range(&text).unwrap();
    for (wanted, matches) in [("true", true), ("false", false)] {
        let wanted = Variant::text(wanted).unwrap();
        // SAFETY: `range` is an ITextRangeProvider; the VARIANT passes by
        // value and stays `wanted`'s, which clears it.
        let found = interface(|out| unsafe {
            (range_vt(&range).FindAttribute)(
                range.as_raw(),
                UIA_IsReadOnlyAttributeId,
                wanted.by_value(),
                0,
                out,
            )
        })
        .unwrap();
        assert_eq!(found.is_some(), matches, "the terminal's text is read-only");
    }
}

#[test]
fn nullable_native_results_write_null_or_an_owned_interface_and_reject_null_slots() {
    let (shared, _) = fixture();
    let root = shared.root().unwrap();
    let simple = vt::<IRawElementProviderSimple_Vtbl>(&root);
    let sentinel = ptr::dangling_mut::<c_void>();
    let mut out = sentinel;
    // SAFETY (each call below): the interface each pointer was queried as,
    // with a writable or a null out slot.
    unsafe {
        assert_eq!(
            (simple.GetPatternProvider)(root.as_raw(), UIA_ValuePatternId, &mut out),
            0
        );
        assert!(
            out.is_null(),
            "S_OK without a pattern initializes the caller's slot"
        );
        assert_eq!(
            (simple.GetPatternProvider)(root.as_raw(), UIA_TextPattern2Id, &mut out),
            0
        );
        let pattern = Com::from_raw(out).expect("a pattern");
        assert!(
            pattern.query(&IID_ITextProvider2).is_ok(),
            "returned reference is owned and queryable"
        );
        assert_eq!(
            (simple.GetPatternProvider)(root.as_raw(), UIA_TextPattern2Id, ptr::null_mut()),
            E_POINTER
        );
    }
    let fragment = root.query(&IID_IRawElementProviderFragment).unwrap();
    let fragment_vt = vt::<IRawElementProviderFragment_Vtbl>(&fragment);
    let mut runtime = ptr::dangling_mut::<SAFEARRAY>();
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            (fragment_vt.GetRuntimeId)(fragment.as_raw(), &mut runtime),
            0
        );
        assert!(
            runtime.is_null(),
            "the HWND root delegates its runtime id to its host"
        );
        assert_eq!(
            (fragment_vt.GetRuntimeId)(fragment.as_raw(), ptr::null_mut()),
            E_POINTER
        );
    }
    out = sentinel;
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            (fragment_vt.Navigate)(fragment.as_raw(), NavigateDirection_Parent, &mut out),
            0
        );
        assert!(out.is_null(), "the fragment root has no parent");
        assert_eq!(
            (fragment_vt.Navigate)(fragment.as_raw(), NavigateDirection_Parent, ptr::null_mut()),
            E_POINTER
        );
    }
    let child = shared.provider(NodeId(model::NodeId(3))).unwrap();
    out = sentinel;
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            (vt::<IRawElementProviderSimple_Vtbl>(&child).HostRawElementProvider)(
                child.as_raw(),
                &mut out
            ),
            0
        );
        assert!(out.is_null(), "a fragment child has no host HWND provider");
    }
    let mut selectable = update(true);
    selectable.nodes[2].1.set_selected(false);
    shared
        .state
        .lock_or_recover()
        .publication
        .update(selectable);
    let item = shared
        .provider(NodeId(model::NodeId(3)))
        .unwrap()
        .query(&IID_ISelectionItemProvider)
        .unwrap();
    let item_vt = vt::<ISelectionItemProvider_Vtbl>(&item);
    out = sentinel;
    // SAFETY: as above.
    unsafe {
        assert_eq!((item_vt.SelectionContainer)(item.as_raw(), &mut out), 0);
        assert!(
            out.is_null(),
            "a selected node without a selection container returns null"
        );
        assert_eq!(
            (item_vt.SelectionContainer)(item.as_raw(), ptr::null_mut()),
            E_POINTER
        );
    }
    let text = root.query(&IID_ITextProvider2).unwrap();
    let range = document_range(&text).unwrap();
    let needle = Bstr::new("absent").unwrap();
    out = sentinel;
    // SAFETY: as above; `needle` stays ours.
    unsafe {
        let find = range_vt(&range).FindText;
        assert_eq!(
            find(
                range.as_raw(),
                needle.as_raw().cast_mut().cast(),
                0,
                0,
                &mut out
            ),
            0
        );
        assert!(out.is_null(), "an unmatched search returns S_OK/null");
        assert_eq!(
            find(
                range.as_raw(),
                needle.as_raw().cast_mut().cast(),
                0,
                0,
                ptr::null_mut()
            ),
            E_POINTER
        );
    }
}

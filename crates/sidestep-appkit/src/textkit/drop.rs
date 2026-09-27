//! A text view as a drag destination: text dropped on an editable text
//! view goes in where `characterIndexForInsertionAtPoint:` says (asked by
//! message when a subclass overrides it, as AppKit's view asks it in each
//! step of a drag), and ends up selected.
//!
//! The view takes the types `acceptableDragTypes` names (plain text is the
//! one it reads) from the start, though a new view's
//! `registeredDraggedTypes` is empty, as on macOS: the drag machinery asks
//! [`drop_types`]. `updateDragTypeRegistration`, which `setEditable:` (and
//! `setSelectable:` turning editing off) sends when the editable flag
//! changes, registers them while the view is editable and none while it
//! isn't, as AppKit's does. Wayland shows no drop caret, so there is none
//! to draw.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{ClassType, define_class, msg_send};
use objc2_app_kit::{NSDragOperation, NSDraggingInfo, NSPasteboardTypeString, NSTextView};
use objc2_foundation::{NSArray, NSRange, NSString};

use super::edit::Kind;
use super::text_view::{NSTextViewImpl, as_impl};

fn view(this: &AnyObject) -> &NSTextViewImpl {
    as_impl(this).expect("sidestep: a text view method on a text view")
}

/// Whether the drag has text an editable view would take.
fn takes(v: &NSTextViewImpl, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
    // SAFETY: the constant is a string AppKit exports.
    let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]);
    v.is_editable_now() && info.draggingPasteboard().availableTypeFromArray(&types).is_some()
}

/// Where a drop would go, for the drag's place.
fn index_for(v: &NSTextViewImpl, info: &ProtocolObject<dyn NSDraggingInfo>) -> usize {
    let at = v.as_text_view().convertPoint_fromView(info.draggingLocation(), None);
    v.drop_index(at).min(v.text_length())
}

/// What the view does with the drag: a copy, when it takes it.
fn operation(v: &NSTextViewImpl, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
    if !takes(v, info) {
        return NSDragOperation::None;
    }
    index_for(v, info);
    let allowed = info.draggingSourceOperationMask();
    if allowed.contains(NSDragOperation::Copy) {
        NSDragOperation::Copy
    } else if allowed.contains(NSDragOperation::Generic) {
        NSDragOperation::Generic
    } else {
        NSDragOperation::None
    }
}

/// The text goes in at the drop, which leaves it selected. Whether it
/// went in.
fn drop_text(v: &NSTextViewImpl, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
    if !takes(v, info) {
        return false;
    }
    // SAFETY: the constant is a string AppKit exports.
    let Some(text) = info.draggingPasteboard().stringForType(unsafe { NSPasteboardTypeString }) else {
        return false;
    };
    let at = index_for(v, info);
    v.set_selection_internal(NSRange::new(at, 0), false);
    if !v.edit_replace_ns(NSRange::new(at, 0), &text, Kind::Other) {
        return false;
    }
    v.set_selection_internal(NSRange::new(at, text.length()), false);
    true
}

define_class!(
    // `self` is a text view in these (see the category below).
    #[unsafe(super(NSObject))]
    #[name = "_SidestepTextViewDrop"]
    struct TextViewDrop;

    impl TextViewDrop {
        #[unsafe(method_id(acceptableDragTypes))]
        fn acceptable_drag_types(&self) -> Retained<NSArray<NSString>> {
            // SAFETY: the constant is a string AppKit exports.
            NSArray::from_slice(&[unsafe { NSPasteboardTypeString }])
        }

        /// Register `acceptableDragTypes` (asked by message) while the
        /// view is editable, none while it isn't, and take drops only
        /// then.
        #[unsafe(method(updateDragTypeRegistration))]
        fn update_drag_type_registration(&self) {
            let this: &AnyObject = self;
            let v = view(this);
            let editable = v.is_editable_now();
            v.set_takes_drops(editable);
            if editable {
                // SAFETY: acceptableDragTypes returns an array of types,
                // which registerForDraggedTypes: takes.
                unsafe {
                    let types: Retained<NSArray<NSString>> = msg_send![this, acceptableDragTypes];
                    let _: () = msg_send![this, registerForDraggedTypes: &*types];
                }
            } else {
                // SAFETY: unregisterDraggedTypes takes nothing.
                let _: () = unsafe { msg_send![this, unregisterDraggedTypes] };
            }
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            operation(view(self), info)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            operation(view(self), info)
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _info: Option<&ProtocolObject<dyn NSDraggingInfo>>) {}

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let v = view(self);
            index_for(v, info);
            takes(v, info)
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            drop_text(view(self), info)
        }

        #[unsafe(method(concludeDragOperation:))]
        fn conclude_drag_operation(&self, _info: Option<&ProtocolObject<dyn NSDraggingInfo>>) {}
    }
);

sidestep_runtime::category!("NSTextView"(SidestepTextDrop), |category| {
    // SAFETY: the helper's methods treat their receiver as a text view.
    unsafe { category.add_methods_of(TextViewDrop::class()) };
});

/// The types a text view takes in a drop: `acceptableDragTypes` (asked by
/// message, as a subclass may name others) while it takes drops; `None`
/// for other objects.
pub(crate) fn drop_types(object: &AnyObject) -> Option<Retained<NSArray<NSString>>> {
    if !as_impl(object)?.takes_drops() {
        return None;
    }
    // SAFETY: acceptableDragTypes returns an array of types.
    Some(unsafe { msg_send![object, acceptableDragTypes] })
}

/// Register the view's drag types afresh, by message (a subclass may
/// override it), as `setEditable:` does.
pub(crate) fn update_registration(view: &NSTextView) {
    // SAFETY: updateDragTypeRegistration takes nothing.
    let _: () = unsafe { msg_send![view, updateDragTypeRegistration] };
}

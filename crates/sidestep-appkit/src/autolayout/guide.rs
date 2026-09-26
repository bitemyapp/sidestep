//! `NSLayoutGuide`: a rectangle constraints can place, with no view behind
//! it. Its owning view holds it, and lets go of it (and clears the link
//! back) when it goes; its frame is in that view's coordinates, read from
//! the solver.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutConstraintOrientation, NSLayoutDimension, NSLayoutGuide,
    NSLayoutXAxisAnchor, NSLayoutYAxisAnchor, NSView,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSRect, NSString};

use super::ItemRef;

pub(crate) struct GuideIvars {
    /// Unretained: the owning view holds the guide, and clears this when
    /// it goes.
    owner: Cell<Option<NonNull<NSView>>>,
    identifier: RefCell<Retained<NSString>>,
    anchors: RefCell<super::anchor::Anchors>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSLayoutGuide"]
    #[ivars = GuideIvars]
    pub(crate) struct NSLayoutGuideImpl;

    impl NSLayoutGuideImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(GuideIvars {
                owner: Cell::new(None),
                identifier: RefCell::new(NSString::new()),
                anchors: RefCell::default(),
            });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(frame))]
        fn frame(&self) -> NSRect {
            super::guide_frame(as_guide(self))
        }

        #[unsafe(method_id(owningView))]
        fn owning_view(&self) -> Option<Retained<NSView>> {
            // SAFETY: the owning view holds the guide, so outlives the link.
            owner(as_guide(self)).map(|v| objc2::Message::retain(unsafe { v.as_ref() }))
        }

        #[unsafe(method(setOwningView:))]
        fn set_owning_view(&self, view: Option<&NSView>) {
            match view {
                Some(view) => super::add_guide(view, as_guide(self)),
                None => {
                    if let Some(owner) = owner(as_guide(self)) {
                        // SAFETY: as in owningView.
                        super::remove_guide(unsafe { owner.as_ref() }, as_guide(self));
                    }
                }
            }
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Retained<NSString> {
            self.ivars().identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: &NSString) {
            let old = self.ivars().identifier.replace(objc2_foundation::NSCopying::copy(identifier));
            drop(old);
        }

        #[unsafe(method_id(leadingAnchor))]
        fn leading_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Leading)
        }

        #[unsafe(method_id(trailingAnchor))]
        fn trailing_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Trailing)
        }

        #[unsafe(method_id(leftAnchor))]
        fn left_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Left)
        }

        #[unsafe(method_id(rightAnchor))]
        fn right_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Right)
        }

        #[unsafe(method_id(topAnchor))]
        fn top_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::Top)
        }

        #[unsafe(method_id(bottomAnchor))]
        fn bottom_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::Bottom)
        }

        #[unsafe(method_id(widthAnchor))]
        fn width_anchor(&self) -> Retained<NSLayoutDimension> {
            anchor(self, NSLayoutAttribute::Width)
        }

        #[unsafe(method_id(heightAnchor))]
        fn height_anchor(&self) -> Retained<NSLayoutDimension> {
            anchor(self, NSLayoutAttribute::Height)
        }

        #[unsafe(method_id(centerXAnchor))]
        fn center_x_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::CenterX)
        }

        #[unsafe(method_id(centerYAnchor))]
        fn center_y_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::CenterY)
        }

        #[unsafe(method(hasAmbiguousLayout))]
        fn has_ambiguous_layout(&self) -> bool {
            super::item_is_ambiguous(item(as_guide(self)))
        }

        #[unsafe(method_id(constraintsAffectingLayoutForOrientation:))]
        fn constraints_affecting_layout(
            &self,
            orientation: NSLayoutConstraintOrientation,
        ) -> Retained<NSArray<NSLayoutConstraint>> {
            super::constraints_affecting(item(as_guide(self)), Some(super::Axis::of(orientation)))
        }
    }

    unsafe impl NSObjectProtocol for NSLayoutGuideImpl {}
);

fn imp(guide: &NSLayoutGuide) -> &NSLayoutGuideImpl {
    // SAFETY: NSLayoutGuide is NSLayoutGuideImpl's class; subclasses share
    // its layout.
    unsafe { &*(guide as *const NSLayoutGuide).cast::<NSLayoutGuideImpl>() }
}

fn as_guide(guide: &NSLayoutGuideImpl) -> &NSLayoutGuide {
    // SAFETY: as in `imp`.
    unsafe { &*(guide as *const NSLayoutGuideImpl).cast::<NSLayoutGuide>() }
}

pub(crate) fn owner(guide: &NSLayoutGuide) -> Option<NonNull<NSView>> {
    imp(guide).ivars().owner.get()
}

pub(crate) fn set_owner(guide: &NSLayoutGuide, view: Option<NonNull<NSView>>) {
    imp(guide).ivars().owner.set(view);
}

pub(crate) fn item(guide: &NSLayoutGuide) -> ItemRef {
    ItemRef::guide(guide)
}

/// Work with the anchors the guide keeps.
pub(crate) fn with_anchors<R>(guide: &NSLayoutGuide, f: impl FnOnce(&mut super::anchor::Anchors) -> R) -> R {
    f(&mut imp(guide).ivars().anchors.borrow_mut())
}

fn anchor<T: objc2::Message>(guide: &NSLayoutGuideImpl, attr: NSLayoutAttribute) -> Retained<T> {
    let g = as_guide(guide);
    let object: &AnyObject = g;
    // SAFETY: `make` returns the anchor class the attribute's axis takes,
    // which the caller asked for.
    unsafe { Retained::cast_unchecked(super::anchor::of(object, item(g), attr)) }
}

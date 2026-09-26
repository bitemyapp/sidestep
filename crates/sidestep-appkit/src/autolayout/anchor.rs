//! `NSLayoutAnchor` and its three kinds: `NSLayoutXAxisAnchor`,
//! `NSLayoutYAxisAnchor` and `NSLayoutDimension`.
//!
//! objc2 makes NSLayoutAnchor generic over the kind; the runtime sees one
//! class with three subclasses, each an attribute of an item (a view or a
//! layout guide). An anchor made by `anchorWithOffsetToAnchor:` is a
//! dimension too: the distance between two anchors, which has no item or
//! name of its own.
//!
//! As in AppKit, an item hands out the same anchor object for an attribute
//! every time, and a constraint's `firstAnchor` is that object too, so
//! anchors compare equal by identity. The item keeps its anchors (`of`),
//! and an anchor names its item weakly, so nothing cycles; an anchor whose
//! item is gone has no item and makes no constraints.

use std::cell::RefCell;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSLayoutAnchor, NSLayoutAttribute, NSLayoutConstraint, NSLayoutDimension, NSLayoutRelation, NSLayoutXAxisAnchor,
    NSLayoutYAxisAnchor,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};

use super::{ItemRef, Term};

/// What an anchor stands for.
#[derive(Clone)]
pub(crate) enum AnchorKind {
    /// An attribute of an item, which keeps the anchor.
    Item(Weak<AnyObject>, ItemRef, NSLayoutAttribute),
    /// The distance from the first anchor to the second.
    Distance(Retained<NSLayoutAnchor>, Retained<NSLayoutAnchor>),
}

#[derive(Default)]
pub(crate) struct AnchorIvars {
    kind: RefCell<Option<AnchorKind>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSLayoutAnchor"]
    #[ivars = AnchorIvars]
    pub(crate) struct NSLayoutAnchorImpl;

    impl NSLayoutAnchorImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(AnchorIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(constraintEqualToAnchor:))]
        fn equal(&self, anchor: &NSLayoutAnchor) -> Retained<NSLayoutConstraint> {
            relate(self, NSLayoutRelation::Equal, anchor, 1.0, 0.0)
        }

        #[unsafe(method_id(constraintGreaterThanOrEqualToAnchor:))]
        fn greater(&self, anchor: &NSLayoutAnchor) -> Retained<NSLayoutConstraint> {
            relate(self, NSLayoutRelation::GreaterThanOrEqual, anchor, 1.0, 0.0)
        }

        #[unsafe(method_id(constraintLessThanOrEqualToAnchor:))]
        fn less(&self, anchor: &NSLayoutAnchor) -> Retained<NSLayoutConstraint> {
            relate(self, NSLayoutRelation::LessThanOrEqual, anchor, 1.0, 0.0)
        }

        #[unsafe(method_id(constraintEqualToAnchor:constant:))]
        fn equal_constant(&self, anchor: &NSLayoutAnchor, c: f64) -> Retained<NSLayoutConstraint> {
            relate(self, NSLayoutRelation::Equal, anchor, 1.0, c)
        }

        #[unsafe(method_id(constraintGreaterThanOrEqualToAnchor:constant:))]
        fn greater_constant(&self, anchor: &NSLayoutAnchor, c: f64) -> Retained<NSLayoutConstraint> {
            relate(self, NSLayoutRelation::GreaterThanOrEqual, anchor, 1.0, c)
        }

        #[unsafe(method_id(constraintLessThanOrEqualToAnchor:constant:))]
        fn less_constant(&self, anchor: &NSLayoutAnchor, c: f64) -> Retained<NSLayoutConstraint> {
            relate(self, NSLayoutRelation::LessThanOrEqual, anchor, 1.0, c)
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            let name = match kind(self) {
                AnchorKind::Item(_, _, attr) => super::attribute_name(attr),
                AnchorKind::Distance(..) => "",
            };
            NSString::from_str(name)
        }

        #[unsafe(method_id(item))]
        fn item(&self) -> Option<Retained<AnyObject>> {
            match kind(self) {
                AnchorKind::Item(item, ..) => item.load(),
                AnchorKind::Distance(..) => None,
            }
        }

        #[unsafe(method(hasAmbiguousLayout))]
        fn has_ambiguous_layout(&self) -> bool {
            match kind(self) {
                // The loaded item keeps `item`'s address alive.
                AnchorKind::Item(object, item, _) => object.load().is_some_and(|_o| super::item_is_ambiguous(item)),
                AnchorKind::Distance(..) => false,
            }
        }

        #[unsafe(method_id(constraintsAffectingLayout))]
        fn constraints_affecting_layout(&self) -> Retained<NSArray<NSLayoutConstraint>> {
            match kind(self) {
                AnchorKind::Item(object, item, attr) => match object.load() {
                    Some(_o) => super::constraints_affecting(item, super::axis_of(attr)),
                    None => NSArray::new(),
                },
                AnchorKind::Distance(..) => NSArray::new(),
            }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut std::ffi::c_void) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSLayoutAnchorImpl {}
);

define_class!(
    #[unsafe(super(NSLayoutAnchor, NSObject))]
    #[name = "NSLayoutXAxisAnchor"]
    pub(crate) struct NSLayoutXAxisAnchorImpl;

    impl NSLayoutXAxisAnchorImpl {
        #[unsafe(method_id(anchorWithOffsetToAnchor:))]
        fn anchor_with_offset_to_anchor(&self, other: &NSLayoutXAxisAnchor) -> Retained<NSLayoutDimension> {
            distance(imp(self), other)
        }

        #[unsafe(method_id(constraintEqualToSystemSpacingAfterAnchor:multiplier:))]
        fn system_equal(&self, anchor: &NSLayoutXAxisAnchor, m: f64) -> Retained<NSLayoutConstraint> {
            spacing(imp(self), NSLayoutRelation::Equal, anchor, m)
        }

        #[unsafe(method_id(constraintGreaterThanOrEqualToSystemSpacingAfterAnchor:multiplier:))]
        fn system_greater(&self, anchor: &NSLayoutXAxisAnchor, m: f64) -> Retained<NSLayoutConstraint> {
            spacing(imp(self), NSLayoutRelation::GreaterThanOrEqual, anchor, m)
        }

        #[unsafe(method_id(constraintLessThanOrEqualToSystemSpacingAfterAnchor:multiplier:))]
        fn system_less(&self, anchor: &NSLayoutXAxisAnchor, m: f64) -> Retained<NSLayoutConstraint> {
            spacing(imp(self), NSLayoutRelation::LessThanOrEqual, anchor, m)
        }
    }
);

define_class!(
    #[unsafe(super(NSLayoutAnchor, NSObject))]
    #[name = "NSLayoutYAxisAnchor"]
    pub(crate) struct NSLayoutYAxisAnchorImpl;

    impl NSLayoutYAxisAnchorImpl {
        #[unsafe(method_id(anchorWithOffsetToAnchor:))]
        fn anchor_with_offset_to_anchor(&self, other: &NSLayoutYAxisAnchor) -> Retained<NSLayoutDimension> {
            distance(imp(self), other)
        }

        #[unsafe(method_id(constraintEqualToSystemSpacingBelowAnchor:multiplier:))]
        fn system_equal(&self, anchor: &NSLayoutYAxisAnchor, m: f64) -> Retained<NSLayoutConstraint> {
            spacing(imp(self), NSLayoutRelation::Equal, anchor, m)
        }

        #[unsafe(method_id(constraintGreaterThanOrEqualToSystemSpacingBelowAnchor:multiplier:))]
        fn system_greater(&self, anchor: &NSLayoutYAxisAnchor, m: f64) -> Retained<NSLayoutConstraint> {
            spacing(imp(self), NSLayoutRelation::GreaterThanOrEqual, anchor, m)
        }

        #[unsafe(method_id(constraintLessThanOrEqualToSystemSpacingBelowAnchor:multiplier:))]
        fn system_less(&self, anchor: &NSLayoutYAxisAnchor, m: f64) -> Retained<NSLayoutConstraint> {
            spacing(imp(self), NSLayoutRelation::LessThanOrEqual, anchor, m)
        }
    }
);

define_class!(
    #[unsafe(super(NSLayoutAnchor, NSObject))]
    #[name = "NSLayoutDimension"]
    pub(crate) struct NSLayoutDimensionImpl;

    impl NSLayoutDimensionImpl {
        #[unsafe(method_id(constraintEqualToConstant:))]
        fn equal_constant(&self, c: f64) -> Retained<NSLayoutConstraint> {
            to_constant(self, NSLayoutRelation::Equal, c)
        }

        #[unsafe(method_id(constraintGreaterThanOrEqualToConstant:))]
        fn greater_constant(&self, c: f64) -> Retained<NSLayoutConstraint> {
            to_constant(self, NSLayoutRelation::GreaterThanOrEqual, c)
        }

        #[unsafe(method_id(constraintLessThanOrEqualToConstant:))]
        fn less_constant(&self, c: f64) -> Retained<NSLayoutConstraint> {
            to_constant(self, NSLayoutRelation::LessThanOrEqual, c)
        }

        #[unsafe(method_id(constraintEqualToAnchor:multiplier:))]
        fn equal_multiplier(&self, anchor: &NSLayoutDimension, m: f64) -> Retained<NSLayoutConstraint> {
            relate(imp(self), NSLayoutRelation::Equal, anchor, m, 0.0)
        }

        #[unsafe(method_id(constraintGreaterThanOrEqualToAnchor:multiplier:))]
        fn greater_multiplier(&self, anchor: &NSLayoutDimension, m: f64) -> Retained<NSLayoutConstraint> {
            relate(imp(self), NSLayoutRelation::GreaterThanOrEqual, anchor, m, 0.0)
        }

        #[unsafe(method_id(constraintLessThanOrEqualToAnchor:multiplier:))]
        fn less_multiplier(&self, anchor: &NSLayoutDimension, m: f64) -> Retained<NSLayoutConstraint> {
            relate(imp(self), NSLayoutRelation::LessThanOrEqual, anchor, m, 0.0)
        }

        #[unsafe(method_id(constraintEqualToAnchor:multiplier:constant:))]
        fn equal_multiplier_constant(&self, anchor: &NSLayoutDimension, m: f64, c: f64) -> Retained<NSLayoutConstraint> {
            relate(imp(self), NSLayoutRelation::Equal, anchor, m, c)
        }

        #[unsafe(method_id(constraintGreaterThanOrEqualToAnchor:multiplier:constant:))]
        fn greater_multiplier_constant(
            &self,
            anchor: &NSLayoutDimension,
            m: f64,
            c: f64,
        ) -> Retained<NSLayoutConstraint> {
            relate(imp(self), NSLayoutRelation::GreaterThanOrEqual, anchor, m, c)
        }

        #[unsafe(method_id(constraintLessThanOrEqualToAnchor:multiplier:constant:))]
        fn less_multiplier_constant(&self, anchor: &NSLayoutDimension, m: f64, c: f64) -> Retained<NSLayoutConstraint> {
            relate(imp(self), NSLayoutRelation::LessThanOrEqual, anchor, m, c)
        }
    }
);

/// Any anchor as the class that keeps anchors' state.
fn imp<T: ?Sized>(anchor: &T) -> &NSLayoutAnchorImpl {
    // SAFETY: every anchor is an instance of NSLayoutAnchor or one of its
    // subclasses, whose layout NSLayoutAnchorImpl describes.
    unsafe { &*(anchor as *const T).cast::<NSLayoutAnchorImpl>() }
}

fn kind<T: ?Sized>(anchor: &T) -> AnchorKind {
    imp(anchor).ivars().kind.borrow().clone().expect("sidestep: an NSLayoutAnchor made without an item")
}

/// The terms an anchor stands for, each an item's attribute with a sign,
/// with the items.
pub(crate) fn terms<T: ?Sized>(anchor: &T) -> Vec<(Retained<AnyObject>, Term)> {
    match kind(anchor) {
        AnchorKind::Item(object, item, attr) => {
            let Some(object) = object.load() else {
                panic!("sidestep: a layout anchor whose view or layout guide is gone makes no constraints")
            };
            vec![(object, Term { item, attr, coeff: 1.0 })]
        }
        AnchorKind::Distance(from, to) => {
            let mut all = terms(&*to);
            all.extend(terms(&*from).into_iter().map(|(o, t)| (o, Term { coeff: -t.coeff, ..t })));
            all
        }
    }
}

/// Where an item keeps its anchor for an attribute.
pub(crate) type Anchors = [Option<Retained<NSLayoutAnchor>>; 12];

/// The anchor for `attr` of `item` (`object`), made when first asked for
/// and kept by the item.
pub(crate) fn of(object: &AnyObject, item: ItemRef, attr: NSLayoutAttribute) -> Retained<NSLayoutAnchor> {
    let slot = usize::try_from(attr.0 - 1).ok().filter(|&i| i < 12).expect("an anchor's attribute");
    // SAFETY: `object` is the item, alive while it's borrowed.
    let kept = unsafe { super::with_anchors(item, |anchors| anchors[slot].clone()) };
    if let Some(anchor) = kept {
        return anchor;
    }
    let anchor = make(object, item, attr);
    // SAFETY: as above.
    unsafe { super::with_anchors(item, |anchors| anchors[slot] = Some(anchor.clone())) };
    anchor
}

/// A new anchor for `attr` of `item`, of the class that attribute's axis
/// takes.
fn make(object: &AnyObject, item: ItemRef, attr: NSLayoutAttribute) -> Retained<NSLayoutAnchor> {
    let anchor: Retained<NSLayoutAnchor> = match super::axis_of(attr) {
        // SAFETY: init is the anchors' initializer.
        Some(super::Axis::X) => unsafe {
            Retained::cast_unchecked(NSLayoutXAxisAnchor::init(NSLayoutXAxisAnchor::alloc()))
        },
        Some(super::Axis::Y) => unsafe {
            Retained::cast_unchecked(NSLayoutYAxisAnchor::init(NSLayoutYAxisAnchor::alloc()))
        },
        // SAFETY: as above.
        None => unsafe { Retained::cast_unchecked(NSLayoutDimension::init(NSLayoutDimension::alloc())) },
    };
    imp(&*anchor).ivars().kind.replace(Some(AnchorKind::Item(Weak::new(object), item, attr)));
    anchor
}

fn distance<T: ?Sized>(from: &NSLayoutAnchorImpl, to: &T) -> Retained<NSLayoutDimension> {
    // SAFETY: both are anchors.
    let (from, to): (Retained<NSLayoutAnchor>, Retained<NSLayoutAnchor>) =
        unsafe { (Retained::cast_unchecked(from.retain()), Retained::cast_unchecked(imp(to).retain())) };
    distance_between(&from, &to)
}

/// The dimension from `from` to `to` (`anchorWithOffsetToAnchor:`).
pub(crate) fn distance_between(from: &NSLayoutAnchor, to: &NSLayoutAnchor) -> Retained<NSLayoutDimension> {
    let dimension = NSLayoutDimension::init(NSLayoutDimension::alloc());
    let kind = AnchorKind::Distance(from.retain(), to.retain());
    imp(&*dimension).ivars().kind.replace(Some(kind));
    dimension
}

/// `first relation second × multiplier + constant`.
fn relate<T: ?Sized>(
    first: &NSLayoutAnchorImpl,
    relation: NSLayoutRelation,
    second: &T,
    multiplier: f64,
    constant: f64,
) -> Retained<NSLayoutConstraint> {
    super::constraint::make(terms(first), relation, Some(terms(second)), multiplier, constant)
}

fn to_constant(first: &NSLayoutDimensionImpl, relation: NSLayoutRelation, c: f64) -> Retained<NSLayoutConstraint> {
    super::constraint::make(terms(first), relation, None, 1.0, c)
}

/// The standard space between two items, in points, as macOS gives it
/// between views that aren't text.
pub(crate) const SYSTEM_SPACING: f64 = 8.0;

/// `first relation second + spacing × multiplier`, the space measured from
/// `anchor` onwards.
fn spacing<T: ?Sized>(
    first: &NSLayoutAnchorImpl,
    relation: NSLayoutRelation,
    anchor: &T,
    multiplier: f64,
) -> Retained<NSLayoutConstraint> {
    relate(first, relation, anchor, 1.0, SYSTEM_SPACING * multiplier)
}

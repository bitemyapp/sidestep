//! `NSLayoutConstraint`: `first relation second × multiplier + constant`,
//! each side a sum of item attributes (one, except for distances between
//! anchors), at a priority.
//!
//! A constraint names its items weakly, as AppKit's do: a view keeps the
//! constraints installed on it, and a constraint on a single view is
//! installed on that view. While a constraint is active its items are in
//! the tree of the view it's installed on, which keeps them alive; that is
//! what lets the engine use their addresses. A view leaving the tree takes
//! the constraints that cross its edge out, and a view that goes away lets
//! go of the constraints installed on it, which are then inactive.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSLayoutAnchor, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSView};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};

use super::{ItemRef, Term};

/// One side's term as a constraint keeps it.
struct Held {
    item: Weak<AnyObject>,
    term: Term,
}

pub(crate) struct ConstraintIvars {
    first: RefCell<Vec<Held>>,
    second: RefCell<Vec<Held>>,
    relation: Cell<NSLayoutRelation>,
    multiplier: Cell<f64>,
    constant: Cell<f64>,
    priority: Cell<f32>,
    identifier: RefCell<Option<Retained<NSString>>>,
    /// The view it's installed on while active, which holds it and clears
    /// this when it goes.
    installed: Cell<Option<NonNull<NSView>>>,
    archived: Cell<bool>,
}

impl Default for ConstraintIvars {
    fn default() -> Self {
        ConstraintIvars {
            first: RefCell::default(),
            second: RefCell::default(),
            relation: Cell::new(NSLayoutRelation::Equal),
            multiplier: Cell::new(1.0),
            constant: Cell::new(0.0),
            priority: Cell::new(1000.0),
            identifier: RefCell::default(),
            installed: Cell::new(None),
            archived: Cell::new(false),
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSLayoutConstraint"]
    #[ivars = ConstraintIvars]
    pub(crate) struct NSLayoutConstraintImpl;

    impl NSLayoutConstraintImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ConstraintIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(constraintWithItem:attribute:relatedBy:toItem:attribute:multiplier:constant:))]
        fn with_item(
            first: &AnyObject,
            first_attr: NSLayoutAttribute,
            relation: NSLayoutRelation,
            second: Option<&AnyObject>,
            second_attr: NSLayoutAttribute,
            multiplier: f64,
            constant: f64,
        ) -> Retained<NSLayoutConstraint> {
            let term = |object: &AnyObject, attr| (object.retain(), Term { item: super::item_ref(object), attr, coeff: 1.0 });
            let second = second.filter(|_| second_attr != NSLayoutAttribute::NotAnAttribute);
            make(vec![term(first, first_attr)], relation, second.map(|s| vec![term(s, second_attr)]), multiplier, constant)
        }

        #[unsafe(method(activateConstraints:))]
        fn activate_constraints(constraints: &NSArray<NSLayoutConstraint>) {
            for c in constraints.iter() {
                super::activate(&c, None);
            }
        }

        #[unsafe(method(deactivateConstraints:))]
        fn deactivate_constraints(constraints: &NSArray<NSLayoutConstraint>) {
            for c in constraints.iter() {
                super::deactivate(&c);
            }
        }

        #[unsafe(method(priority))]
        fn priority(&self) -> f32 {
            self.ivars().priority.get()
        }

        #[unsafe(method(setPriority:))]
        fn set_priority(&self, priority: f32) {
            if self.ivars().priority.replace(priority) != priority {
                super::constraint_changed(as_constraint(self));
            }
        }

        #[unsafe(method(constant))]
        fn constant(&self) -> f64 {
            self.ivars().constant.get()
        }

        #[unsafe(method(setConstant:))]
        fn set_constant(&self, constant: f64) {
            if self.ivars().constant.replace(constant) != constant {
                super::constraint_changed(as_constraint(self));
            }
        }

        #[unsafe(method(multiplier))]
        fn multiplier(&self) -> f64 {
            self.ivars().multiplier.get()
        }

        #[unsafe(method(relation))]
        fn relation(&self) -> NSLayoutRelation {
            self.ivars().relation.get()
        }

        #[unsafe(method_id(firstItem))]
        fn first_item(&self) -> Option<Retained<AnyObject>> {
            single(&self.ivars().first.borrow()).and_then(|h| h.item.load())
        }

        #[unsafe(method_id(secondItem))]
        fn second_item(&self) -> Option<Retained<AnyObject>> {
            single(&self.ivars().second.borrow()).and_then(|h| h.item.load())
        }

        #[unsafe(method(firstAttribute))]
        fn first_attribute(&self) -> NSLayoutAttribute {
            single(&self.ivars().first.borrow()).map_or(NSLayoutAttribute::NotAnAttribute, |h| h.term.attr)
        }

        #[unsafe(method(secondAttribute))]
        fn second_attribute(&self) -> NSLayoutAttribute {
            single(&self.ivars().second.borrow()).map_or(NSLayoutAttribute::NotAnAttribute, |h| h.term.attr)
        }

        #[unsafe(method_id(firstAnchor))]
        fn first_anchor(&self) -> Retained<NSLayoutAnchor> {
            anchor_of(&self.ivars().first.borrow()).expect("sidestep: a constraint without a first item")
        }

        #[unsafe(method_id(secondAnchor))]
        fn second_anchor(&self) -> Option<Retained<NSLayoutAnchor>> {
            anchor_of(&self.ivars().second.borrow())
        }

        #[unsafe(method(isActive))]
        fn is_active(&self) -> bool {
            self.ivars().installed.get().is_some()
        }

        #[unsafe(method(setActive:))]
        fn set_active(&self, active: bool) {
            if active {
                super::activate(as_constraint(self), None);
            } else {
                super::deactivate(as_constraint(self));
            }
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Option<Retained<NSString>> {
            self.ivars().identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: Option<&NSString>) {
            let old = self.ivars().identifier.replace(identifier.map(objc2_foundation::NSCopying::copy));
            drop(old);
        }

        #[unsafe(method(shouldBeArchived))]
        fn should_be_archived(&self) -> bool {
            self.ivars().archived.get()
        }

        #[unsafe(method(setShouldBeArchived:))]
        fn set_should_be_archived(&self, flag: bool) {
            self.ivars().archived.set(flag);
        }
    }

    unsafe impl NSObjectProtocol for NSLayoutConstraintImpl {}
);

pub(crate) fn imp(constraint: &NSLayoutConstraint) -> &NSLayoutConstraintImpl {
    // SAFETY: NSLayoutConstraint is NSLayoutConstraintImpl's class, and
    // subclasses share its layout.
    unsafe { &*(constraint as *const NSLayoutConstraint).cast::<NSLayoutConstraintImpl>() }
}

fn as_constraint(constraint: &NSLayoutConstraintImpl) -> &NSLayoutConstraint {
    // SAFETY: as in `imp`.
    unsafe { &*(constraint as *const NSLayoutConstraintImpl).cast::<NSLayoutConstraint>() }
}

/// A side's one item, if it names exactly one with a coefficient of one.
fn single(side: &[Held]) -> Option<&Held> {
    match side {
        [h] if h.term.coeff == 1.0 => Some(h),
        _ => None,
    }
}

/// A side's anchor: the item's own, as AppKit hands back the anchor the
/// constraint was made from.
fn anchor_of(side: &[Held]) -> Option<Retained<NSLayoutAnchor>> {
    // The loaded item keeps its address, which `term.item` is, alive.
    let own = |h: &Held| h.item.load().map(|item| super::anchor::of(&item, h.term.item, h.term.attr));
    match side {
        [] => None,
        [h] if h.term.coeff == 1.0 => own(h),
        // A distance: from the item subtracted to the one added.
        [to, from] => {
            let (to, from) = (own(to)?, own(from)?);
            // SAFETY: a dimension is an anchor.
            Some(unsafe { Retained::cast_unchecked(super::anchor::distance_between(&from, &to)) })
        }
        _ => None,
    }
}

/// A new constraint, not yet active, over terms of the items given with
/// them.
pub(crate) fn make(
    first: Vec<(Retained<AnyObject>, Term)>,
    relation: NSLayoutRelation,
    second: Option<Vec<(Retained<AnyObject>, Term)>>,
    multiplier: f64,
    constant: f64,
) -> Retained<NSLayoutConstraint> {
    let c = NSLayoutConstraint::init(NSLayoutConstraint::alloc());
    let ivars = imp(&c).ivars();
    let hold = |terms: Vec<(Retained<AnyObject>, Term)>| -> Vec<Held> {
        terms.into_iter().map(|(item, term)| Held { item: Weak::new(&*item), term }).collect()
    };
    ivars.first.replace(hold(first));
    ivars.second.replace(second.map(hold).unwrap_or_default());
    ivars.relation.set(relation);
    ivars.multiplier.set(multiplier);
    ivars.constant.set(constant);
    c
}

/// What the engine reads: both sides' terms, the relation, multiplier,
/// constant and priority, with the items held so the terms' addresses
/// stay good while it reads them.
pub(crate) struct Parts {
    pub first: Vec<Term>,
    pub second: Vec<Term>,
    pub relation: NSLayoutRelation,
    pub multiplier: f64,
    pub constant: f64,
    pub priority: f32,
    _items: Vec<Retained<AnyObject>>,
}

/// The constraint's parts, or none if one of its items is gone.
pub(crate) fn parts(c: &NSLayoutConstraint) -> Option<Parts> {
    let ivars = imp(c).ivars();
    let items = items(c);
    if items.len() != ivars.first.borrow().len() + ivars.second.borrow().len() {
        return None;
    }
    let terms = |side: &RefCell<Vec<Held>>| side.borrow().iter().map(|h| h.term).collect();
    Some(Parts {
        first: terms(&ivars.first),
        second: terms(&ivars.second),
        relation: ivars.relation.get(),
        multiplier: ivars.multiplier.get(),
        constant: ivars.constant.get(),
        priority: ivars.priority.get(),
        _items: items.into_iter().map(|(o, _)| o).collect(),
    })
}

/// Every item the constraint names, while they live.
pub(crate) fn items(c: &NSLayoutConstraint) -> Vec<(Retained<AnyObject>, ItemRef)> {
    let ivars = imp(c).ivars();
    let first = ivars.first.borrow();
    let second = ivars.second.borrow();
    first.iter().chain(second.iter()).filter_map(|h| h.item.load().map(|o| (o, h.term.item))).collect()
}

/// Whether every item the constraint names is alive.
pub(crate) fn items_alive(c: &NSLayoutConstraint) -> bool {
    let ivars = imp(c).ivars();
    let first = ivars.first.borrow();
    let second = ivars.second.borrow();
    first.iter().chain(second.iter()).all(|h| h.item.load().is_some())
}

pub(crate) fn installed(c: &NSLayoutConstraint) -> Option<NonNull<NSView>> {
    imp(c).ivars().installed.get()
}

pub(crate) fn set_installed(c: &NSLayoutConstraint, view: Option<NonNull<NSView>>) {
    imp(c).ivars().installed.set(view);
}

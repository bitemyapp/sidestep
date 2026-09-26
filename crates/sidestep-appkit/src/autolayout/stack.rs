//! `NSStackView`: arranged subviews in a row or a column, placed by
//! constraints the engine makes for the stack (not `NSLayoutConstraint`s,
//! so `constraints` doesn't list them, as on macOS).
//!
//! Along the stack, each distribution is a few linear equations over the
//! arranged views' positions and sizes (`build`): packed in gravity areas,
//! filling (evenly or in proportion to intrinsic sizes), or with equal gaps
//! or equally spaced centers. Across it, the alignment places each view,
//! and clipping resistance keeps views inside. The stack hugs its content
//! just below a view's default hugging, so a stack sized by its content
//! fits it and a view in a stack that is too big doesn't stretch unless it
//! hugs less.
//!
//! Hidden views drop out when `detachesHiddenViews` (the default), and a
//! view whose visibility priority is `NotVisible` is hidden and dropped.
//! When the arranged views hug or resist equally, the first gives way.
//! As on macOS, the insets at the far edge across the stack (bottom of a
//! row, right of a column) don't move views aligned to that edge.

use std::cell::{Cell, RefCell};

use kasuari::{Expression, RelationalOperator, Variable};
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraintOrientation, NSResponder, NSStackView, NSStackViewDelegate,
    NSStackViewDistribution, NSStackViewGravity, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSObjectProtocol, NSRect, NSSize};

use super::engine::{REQUIRED, Spec, Weight};
use crate::views::{self, NSViewImpl};

/// `NSStackViewSpacingUseDefault`: no custom spacing.
const USE_DEFAULT: f64 = f32::MAX as f64;
const MUST_HOLD: f32 = 1000.0;
const NOT_VISIBLE: f32 = 0.0;

/// How hard a stack hugs its content: just below a view's default hugging.
fn default_hugging() -> f32 {
    f32::from_bits(250f32.to_bits() - 1)
}

/// Weaker than any priority, growing with the index: when views hug or
/// resist equally, the first gives way.
fn tie_break(index: usize) -> Weight {
    Weight::Tie(1e-4 * index as f64)
}

struct Arranged {
    view: Retained<NSView>,
    gravity: NSStackViewGravity,
    /// After this view, or `USE_DEFAULT`.
    spacing: f64,
    visibility: f32,
}

pub(crate) struct StackIvars {
    /// In order: leading gravity, then center, then trailing.
    arranged: RefCell<Vec<Arranged>>,
    orientation: Cell<NSUserInterfaceLayoutOrientation>,
    alignment: Cell<NSLayoutAttribute>,
    distribution: Cell<NSStackViewDistribution>,
    spacing: Cell<f64>,
    insets: Cell<NSEdgeInsets>,
    detaches_hidden: Cell<bool>,
    /// Horizontal, vertical.
    hugging: Cell<[f32; 2]>,
    clipping: Cell<[f32; 2]>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
    /// The views dropped at the last layout, to tell the delegate of
    /// changes.
    detached: RefCell<Vec<Retained<NSView>>>,
}

impl Default for StackIvars {
    fn default() -> Self {
        StackIvars {
            arranged: RefCell::default(),
            orientation: Cell::new(NSUserInterfaceLayoutOrientation::Horizontal),
            alignment: Cell::new(NSLayoutAttribute::CenterY),
            distribution: Cell::new(NSStackViewDistribution::GravityAreas),
            spacing: Cell::new(8.0),
            insets: Cell::new(NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }),
            detaches_hidden: Cell::new(true),
            hugging: Cell::new([default_hugging(); 2]),
            clipping: Cell::new([1000.0; 2]),
            delegate: RefCell::new(None),
            detached: RefCell::default(),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSStackView"]
    #[ivars = StackIvars]
    pub(crate) struct NSStackViewImpl;

    impl NSStackViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(StackIvars::default());
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method_id(stackViewWithViews:))]
        fn stack_view_with_views(views: &NSArray<NSView>) -> Retained<NSStackView> {
            let mtm = MainThreadMarker::new().expect("NSStackView on the main thread");
            let stack = NSStackView::new(mtm);
            stack.setTranslatesAutoresizingMaskIntoConstraints(false);
            for v in views.iter() {
                stack.addArrangedSubview(&v);
            }
            stack
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<ProtocolObject<dyn NSStackViewDelegate>>> {
            let d = self.ivars().delegate.borrow().as_ref().and_then(Weak::load);
            // SAFETY: only delegates are stored.
            d.map(|d| unsafe { Retained::cast_unchecked(d) })
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&ProtocolObject<dyn NSStackViewDelegate>>) {
            let weak = delegate.map(|d| Weak::new(d.as_ref()));
            self.ivars().delegate.replace(weak);
        }

        #[unsafe(method(orientation))]
        fn orientation(&self) -> NSUserInterfaceLayoutOrientation {
            self.ivars().orientation.get()
        }

        #[unsafe(method(setOrientation:))]
        fn set_orientation(&self, orientation: NSUserInterfaceLayoutOrientation) {
            if self.ivars().orientation.replace(orientation) != orientation {
                // A centered alignment turns with it.
                let vertical = orientation == NSUserInterfaceLayoutOrientation::Vertical;
                let turned = match self.ivars().alignment.get() {
                    NSLayoutAttribute::CenterY if vertical => NSLayoutAttribute::CenterX,
                    NSLayoutAttribute::CenterX if !vertical => NSLayoutAttribute::CenterY,
                    other => other,
                };
                self.ivars().alignment.set(turned);
                changed(self);
            }
        }

        #[unsafe(method(alignment))]
        fn alignment(&self) -> NSLayoutAttribute {
            self.ivars().alignment.get()
        }

        #[unsafe(method(setAlignment:))]
        fn set_alignment(&self, alignment: NSLayoutAttribute) {
            if self.ivars().alignment.replace(alignment) != alignment {
                changed(self);
            }
        }

        #[unsafe(method(edgeInsets))]
        fn edge_insets(&self) -> NSEdgeInsets {
            self.ivars().insets.get()
        }

        #[unsafe(method(setEdgeInsets:))]
        fn set_edge_insets(&self, insets: NSEdgeInsets) {
            self.ivars().insets.set(insets);
            changed(self);
        }

        #[unsafe(method(distribution))]
        fn distribution(&self) -> NSStackViewDistribution {
            self.ivars().distribution.get()
        }

        #[unsafe(method(setDistribution:))]
        fn set_distribution(&self, distribution: NSStackViewDistribution) {
            if self.ivars().distribution.replace(distribution) != distribution {
                changed(self);
            }
        }

        #[unsafe(method(spacing))]
        fn spacing(&self) -> f64 {
            self.ivars().spacing.get()
        }

        #[unsafe(method(setSpacing:))]
        fn set_spacing(&self, spacing: f64) {
            if self.ivars().spacing.replace(spacing) != spacing {
                changed(self);
            }
        }

        #[unsafe(method(setCustomSpacing:afterView:))]
        fn set_custom_spacing(&self, spacing: f64, view: &NSView) {
            if with_arranged(self, view, |a| a.spacing = spacing) {
                changed(self);
            }
        }

        #[unsafe(method(customSpacingAfterView:))]
        fn custom_spacing_after_view(&self, view: &NSView) -> f64 {
            let arranged = self.ivars().arranged.borrow();
            arranged.iter().find(|a| std::ptr::eq(&*a.view, view)).map_or(USE_DEFAULT, |a| a.spacing)
        }

        #[unsafe(method(detachesHiddenViews))]
        fn detaches_hidden_views(&self) -> bool {
            self.ivars().detaches_hidden.get()
        }

        #[unsafe(method(setDetachesHiddenViews:))]
        fn set_detaches_hidden_views(&self, flag: bool) {
            if self.ivars().detaches_hidden.replace(flag) != flag {
                changed(self);
            }
        }

        #[unsafe(method_id(arrangedSubviews))]
        fn arranged_subviews(&self) -> Retained<NSArray<NSView>> {
            NSArray::from_retained_slice(&arranged(self, |_| true))
        }

        #[unsafe(method(addArrangedSubview:))]
        fn add_arranged_subview(&self, view: &NSView) {
            let at = self.ivars().arranged.borrow().len();
            insert(self, view, at, None);
        }

        #[unsafe(method(insertArrangedSubview:atIndex:))]
        fn insert_arranged_subview(&self, view: &NSView, index: isize) {
            insert(self, view, index.max(0) as usize, None);
        }

        #[unsafe(method(removeArrangedSubview:))]
        fn remove_arranged_subview(&self, view: &NSView) {
            forget(self, view);
        }

        #[unsafe(method_id(detachedViews))]
        fn detached_views(&self) -> Retained<NSArray<NSView>> {
            let detaches = self.ivars().detaches_hidden.get();
            NSArray::from_retained_slice(&arranged(self, |a| detached(a, detaches)))
        }

        #[unsafe(method(setVisibilityPriority:forView:))]
        fn set_visibility_priority(&self, priority: f32, view: &NSView) {
            let mut before = MUST_HOLD;
            if !with_arranged(self, view, |a| before = std::mem::replace(&mut a.visibility, priority)) {
                return;
            }
            // Not visible: hidden and dropped; visible again: shown.
            if priority <= NOT_VISIBLE && before > NOT_VISIBLE {
                view.setHidden(true);
            } else if priority > NOT_VISIBLE && before <= NOT_VISIBLE {
                view.setHidden(false);
            }
            changed(self);
        }

        #[unsafe(method(visibilityPriorityForView:))]
        fn visibility_priority_for_view(&self, view: &NSView) -> f32 {
            let arranged = self.ivars().arranged.borrow();
            arranged.iter().find(|a| std::ptr::eq(&*a.view, view)).map_or(MUST_HOLD, |a| a.visibility)
        }

        #[unsafe(method(clippingResistancePriorityForOrientation:))]
        fn clipping_resistance_priority(&self, orientation: NSLayoutConstraintOrientation) -> f32 {
            self.ivars().clipping.get()[axis(orientation)]
        }

        #[unsafe(method(setClippingResistancePriority:forOrientation:))]
        fn set_clipping_resistance_priority(&self, priority: f32, orientation: NSLayoutConstraintOrientation) {
            let mut p = self.ivars().clipping.get();
            p[axis(orientation)] = priority;
            self.ivars().clipping.set(p);
            changed(self);
        }

        #[unsafe(method(huggingPriorityForOrientation:))]
        fn hugging_priority(&self, orientation: NSLayoutConstraintOrientation) -> f32 {
            self.ivars().hugging.get()[axis(orientation)]
        }

        #[unsafe(method(setHuggingPriority:forOrientation:))]
        fn set_hugging_priority(&self, priority: f32, orientation: NSLayoutConstraintOrientation) {
            let mut p = self.ivars().hugging.get();
            p[axis(orientation)] = priority;
            self.ivars().hugging.set(p);
            changed(self);
        }

        #[unsafe(method(addView:inGravity:))]
        fn add_view_in_gravity(&self, view: &NSView, gravity: NSStackViewGravity) {
            forget(self, view);
            let at = end_of(self, gravity);
            insert(self, view, at, Some(gravity));
        }

        #[unsafe(method(insertView:atIndex:inGravity:))]
        fn insert_view_in_gravity(&self, view: &NSView, index: usize, gravity: NSStackViewGravity) {
            forget(self, view);
            let count = arranged(self, |a| a.gravity == gravity).len();
            let at = start_of(self, gravity) + index.min(count);
            insert(self, view, at, Some(gravity));
        }

        #[unsafe(method(removeView:))]
        fn remove_view(&self, view: &NSView) {
            forget(self, view);
            view.removeFromSuperview();
        }

        #[unsafe(method_id(viewsInGravity:))]
        fn views_in_gravity(&self, gravity: NSStackViewGravity) -> Retained<NSArray<NSView>> {
            NSArray::from_retained_slice(&arranged(self, |a| a.gravity == gravity))
        }

        #[unsafe(method(setViews:inGravity:))]
        fn set_views_in_gravity(&self, views: &NSArray<NSView>, gravity: NSStackViewGravity) {
            for old in arranged(self, |a| a.gravity == gravity) {
                if !views.iter().any(|v| std::ptr::eq(&*v, &*old)) {
                    forget(self, &old);
                    old.removeFromSuperview();
                }
            }
            for v in views.iter() {
                forget(self, &v);
                let at = end_of(self, gravity);
                insert(self, &v, at, Some(gravity));
            }
        }

        #[unsafe(method_id(views))]
        fn views(&self) -> Retained<NSArray<NSView>> {
            NSArray::from_retained_slice(&arranged(self, |_| true))
        }

        #[unsafe(method(hasEqualSpacing))]
        fn has_equal_spacing(&self) -> bool {
            self.ivars().distribution.get() == NSStackViewDistribution::EqualSpacing
        }

        #[unsafe(method(setHasEqualSpacing:))]
        fn set_has_equal_spacing(&self, flag: bool) {
            let d = if flag { NSStackViewDistribution::EqualSpacing } else { NSStackViewDistribution::GravityAreas };
            binding(self).setDistribution(d);
        }

        #[unsafe(method(willRemoveSubview:))]
        fn will_remove_subview(&self, view: &NSView) {
            forget(self, view);
            // SAFETY: NSView's willRemoveSubview: takes the subview.
            unsafe { msg_send![super(self), willRemoveSubview: view] }
        }
    }

    unsafe impl NSObjectProtocol for NSStackViewImpl {}
);

fn binding(this: &NSStackViewImpl) -> &NSStackView {
    // SAFETY: NSStackView is NSStackViewImpl's class.
    unsafe { &*(this as *const NSStackViewImpl).cast::<NSStackView>() }
}

fn as_view(this: &NSStackViewImpl) -> &NSView {
    // SAFETY: an NSStackView is an NSView.
    unsafe { &*(this as *const NSStackViewImpl).cast::<NSView>() }
}

fn axis(orientation: NSLayoutConstraintOrientation) -> usize {
    usize::from(orientation == NSLayoutConstraintOrientation::Vertical)
}

fn with_arranged(this: &NSStackViewImpl, view: &NSView, f: impl FnOnce(&mut Arranged)) -> bool {
    let mut arranged = this.ivars().arranged.borrow_mut();
    match arranged.iter_mut().find(|a| std::ptr::eq(&*a.view, view)) {
        Some(a) => {
            f(a);
            true
        }
        None => false,
    }
}

/// The arranged views `keep` keeps, in order.
fn arranged(this: &NSStackViewImpl, keep: impl Fn(&Arranged) -> bool) -> Vec<Retained<NSView>> {
    this.ivars().arranged.borrow().iter().filter(|a| keep(a)).map(|a| a.view.clone()).collect()
}

/// Where a gravity's views start among the arranged views.
fn start_of(this: &NSStackViewImpl, gravity: NSStackViewGravity) -> usize {
    this.ivars().arranged.borrow().iter().filter(|a| a.gravity.0 < gravity.0).count()
}

/// Just past a gravity's last view.
fn end_of(this: &NSStackViewImpl, gravity: NSStackViewGravity) -> usize {
    this.ivars().arranged.borrow().iter().filter(|a| a.gravity.0 <= gravity.0).count()
}

/// Arrange `view` at `at`, in `gravity` (else that of its neighbours).
fn insert(this: &NSStackViewImpl, view: &NSView, at: usize, gravity: Option<NSStackViewGravity>) {
    forget(this, view);
    {
        let mut arranged = this.ivars().arranged.borrow_mut();
        let at = at.min(arranged.len());
        let gravity = gravity.unwrap_or_else(|| {
            let neighbour = arranged.get(at).or_else(|| at.checked_sub(1).and_then(|i| arranged.get(i)));
            neighbour.map_or(NSStackViewGravity::Leading, |a| a.gravity)
        });
        arranged.insert(at, Arranged { view: view.retain(), gravity, spacing: USE_DEFAULT, visibility: MUST_HOLD });
    }
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    let this_view = as_view(this);
    let own = views::superview_of(views::imp(view)).is_some_and(|s| std::ptr::eq(views::as_view(s), this_view));
    if !own {
        this_view.addSubview(view);
    }
    changed(this);
}

/// Stop arranging `view`; it stays a subview.
fn forget(this: &NSStackViewImpl, view: &NSView) {
    let removed = {
        let mut arranged = this.ivars().arranged.borrow_mut();
        arranged.iter().position(|a| std::ptr::eq(&*a.view, view)).map(|i| arranged.remove(i))
    };
    if removed.is_some() {
        changed(this);
    }
    drop(removed);
}

fn detached(a: &Arranged, detaches_hidden: bool) -> bool {
    a.visibility <= NOT_VISIBLE || detaches_hidden && views::is_hidden(views::imp(&a.view))
}

/// The stack's arrangement changed: its constraints are made again.
fn changed(this: &NSStackViewImpl) {
    super::stack_changed(views::imp(as_view(this)));
}

pub(crate) fn as_stack(view: &NSViewImpl) -> Option<&NSStackViewImpl> {
    views::as_view(view).downcast_ref::<NSStackView>().map(|s| {
        // SAFETY: an NSStackView, whose layout NSStackViewImpl describes.
        unsafe { &*(s as *const NSStackView).cast::<NSStackViewImpl>() }
    })
}

/// Whether `view` is a stack view with arranged subviews: its tree needs
/// an engine.
pub(crate) fn arranges(view: &NSViewImpl) -> bool {
    as_stack(view).is_some_and(|s| !s.ivars().arranged.borrow().is_empty())
}

/// `view`, a subview of a stack, was hidden or shown.
pub(crate) fn subview_hidden(stack: &NSStackViewImpl, view: &NSViewImpl) {
    let arranged = stack.ivars().arranged.borrow().iter().any(|a| std::ptr::eq(views::imp(&a.view), view));
    if arranged && stack.ivars().detaches_hidden.get() {
        changed(stack);
    }
}

// Building the constraints.

/// An arranged view as the constraints see it.
pub(crate) struct Slot {
    pub view: Retained<NSView>,
    pub gravity: NSStackViewGravity,
    /// The space after it.
    pub gap: f64,
    /// Its intrinsic size along the stack, if any.
    pub intrinsic: Option<f64>,
    /// Baseline offsets from its top and its bottom.
    pub baselines: (f64, f64),
}

/// What `build` needs to know of a stack.
pub(crate) struct Plan {
    pub vertical: bool,
    pub alignment: NSLayoutAttribute,
    pub distribution: NSStackViewDistribution,
    pub spacing: f64,
    pub insets: NSEdgeInsets,
    /// Along the stack, then across it.
    pub hugging: [f32; 2],
    pub clipping: [f32; 2],
    pub slots: Vec<Slot>,
}

/// The stack's plan, telling its delegate of views that dropped out or
/// came back.
pub(crate) fn plan(this: &NSStackViewImpl) -> Plan {
    let ivars = this.ivars();
    let vertical = ivars.orientation.get() == NSUserInterfaceLayoutOrientation::Vertical;
    let detaches = ivars.detaches_hidden.get();
    let spacing = ivars.spacing.get();
    let mut attached = Vec::new();
    let mut gone = Vec::new();
    for a in ivars.arranged.borrow().iter() {
        if detached(a, detaches) {
            gone.push(a.view.clone());
        } else {
            let gap = if a.spacing == USE_DEFAULT { spacing } else { a.spacing };
            attached.push((a.view.clone(), a.gravity, gap));
        }
    }
    let alignment = ivars.alignment.get();
    let distribution = ivars.distribution.get();
    let wants_baselines = matches!(alignment, NSLayoutAttribute::FirstBaseline | NSLayoutAttribute::LastBaseline);
    let wants_intrinsic =
        matches!(distribution, NSStackViewDistribution::Fill | NSStackViewDistribution::FillProportionally);
    let slots = attached
        .into_iter()
        .map(|(view, gravity, gap)| {
            let intrinsic = if wants_intrinsic { intrinsic_along(&view, vertical) } else { None };
            let baselines = if wants_baselines {
                (view.firstBaselineOffsetFromTop(), view.lastBaselineOffsetFromBottom())
            } else {
                (0.0, 0.0)
            };
            Slot { view, gravity, gap, intrinsic, baselines }
        })
        .collect();
    tell_delegate(this, gone);
    let [h_hug, v_hug] = ivars.hugging.get();
    let [h_clip, v_clip] = ivars.clipping.get();
    let (hugging, clipping) =
        if vertical { ([v_hug, h_hug], [v_clip, h_clip]) } else { ([h_hug, v_hug], [h_clip, v_clip]) };
    Plan { vertical, alignment, distribution, spacing, insets: ivars.insets.get(), hugging, clipping, slots }
}

fn intrinsic_along(view: &NSView, vertical: bool) -> Option<f64> {
    if !view.respondsToSelector(sel!(intrinsicContentSize)) {
        return None;
    }
    // SAFETY: intrinsicContentSize takes nothing and returns an NSSize.
    let size: NSSize = unsafe { msg_send![view, intrinsicContentSize] };
    let v = if vertical { size.height } else { size.width };
    (v != super::engine::NO_INTRINSIC).then_some(v)
}

fn tell_delegate(this: &NSStackViewImpl, gone: Vec<Retained<NSView>>) {
    let before = this.ivars().detached.replace(gone.clone());
    let has = |list: &[Retained<NSView>], v: &NSView| list.iter().any(|x| std::ptr::eq(&**x, v));
    let newly: Vec<_> = gone.iter().filter(|v| !has(&before, v)).cloned().collect();
    let back: Vec<_> = before.iter().filter(|v| !has(&gone, v)).cloned().collect();
    let Some(delegate) = this.ivars().delegate.borrow().as_ref().and_then(Weak::load) else { return };
    let stack = as_view(this);
    if !newly.is_empty() && delegate.class().responds_to(sel!(stackView:willDetachViews:)) {
        let views = NSArray::from_retained_slice(&newly);
        // SAFETY: the delegate method takes the stack and an array of views.
        let _: () = unsafe { msg_send![&*delegate, stackView: stack, willDetachViews: &*views] };
    }
    if !back.is_empty() && delegate.class().responds_to(sel!(stackView:didReattachViews:)) {
        let views = NSArray::from_retained_slice(&back);
        // SAFETY: as above.
        let _: () = unsafe { msg_send![&*delegate, stackView: stack, didReattachViews: &*views] };
    }
}

/// Position and size along the stack, then across it, from an item's
/// variables `[x, y, w, h]`.
fn axes(vars: [Variable; 4], vertical: bool) -> [Variable; 4] {
    if vertical { [vars[1], vars[3], vars[0], vars[2]] } else { [vars[0], vars[2], vars[1], vars[3]] }
}

/// The stack's constraints, given its variables and those of each slot's
/// view (all in the stack's coordinates, `y` down from its top).
pub(crate) fn build(plan: &Plan, stack: [Variable; 4], views: &[[Variable; 4]]) -> Vec<Spec> {
    use RelationalOperator::{Equal as Eq, GreaterOrEqual as Ge, LessOrEqual as Le};
    let mut out = Vec::new();
    let required = REQUIRED;
    let [_, size, _, cross] = axes(stack, plan.vertical);
    let i = plan.insets;
    let (start_inset, end_inset, cross_start) =
        if plan.vertical { (i.top, i.bottom, i.left) } else { (i.left, i.right, i.top) };
    let clip = Weight::Priority(plan.clipping[0]);
    let clip_cross = Weight::Priority(plan.clipping[1]);
    let mut add = |e: Expression, op: RelationalOperator, w: Weight| out.push(Spec::new(e, op, w));

    // The stack shrinks to its content as far as that allows.
    add(Expression::from(size), Eq, Weight::Priority(plan.hugging[0]));
    add(Expression::from(cross), Eq, Weight::Priority(plan.hugging[1]));
    let n = views.len();
    if n == 0 {
        return out;
    }
    let v: Vec<[Variable; 4]> = views.iter().map(|x| axes(*x, plan.vertical)).collect();
    let pos = |k: usize| Expression::from(v[k][0]);
    let end = |k: usize| v[k][0] + v[k][1];
    let center = |k: usize| v[k][0] + 0.5 * v[k][1];
    let gap = |k: usize| plan.slots[k].gap;
    let first = || pos(0) - start_inset;
    let last = || end(n - 1) - size + end_inset;

    match plan.distribution {
        NSStackViewDistribution::Fill
        | NSStackViewDistribution::FillEqually
        | NSStackViewDistribution::FillProportionally => {
            add(first(), Eq, required);
            add(last(), Eq, required);
            for k in 0..n - 1 {
                add(pos(k + 1) - end(k) - gap(k), Eq, required);
            }
            if plan.distribution == NSStackViewDistribution::FillEqually {
                for k in 1..n {
                    add(v[k][1] - v[0][1], Eq, required);
                }
            } else if plan.distribution == NSStackViewDistribution::FillProportionally {
                let total: f64 = plan.slots.iter().map(|s| s.intrinsic.unwrap_or(0.0).max(0.0)).sum();
                if total > 0.0 {
                    let gaps: f64 = (0..n - 1).map(gap).sum();
                    for (k, slot) in plan.slots.iter().enumerate() {
                        let share = slot.intrinsic.unwrap_or(0.0).max(0.0) / total;
                        // size_k = share × (size − insets − gaps)
                        add(v[k][1] - share * size + share * (start_inset + end_inset + gaps), Eq, required);
                    }
                }
            } else {
                for (k, slot) in plan.slots.iter().enumerate().skip(1) {
                    if let Some(want) = slot.intrinsic {
                        add(v[k][1] - want, Eq, tie_break(k));
                    }
                }
            }
        }
        NSStackViewDistribution::EqualSpacing | NSStackViewDistribution::EqualCentering => {
            add(first(), Eq, required);
            add(last(), Eq, required);
            let step = Variable::new();
            for k in 0..n - 1 {
                add(pos(k + 1) - end(k) - gap(k), Ge, required);
                if plan.distribution == NSStackViewDistribution::EqualSpacing {
                    add(pos(k + 1) - end(k) - step, Eq, required);
                } else {
                    add(center(k + 1) - center(k) - step, Eq, required);
                }
            }
        }
        _ => {
            // Gravity areas: each packed at its edge or in the middle,
            // apart by at least the spacing.
            let gravities = [NSStackViewGravity::Leading, NSStackViewGravity::Center, NSStackViewGravity::Trailing];
            let groups: Vec<Vec<usize>> =
                gravities.iter().map(|g| (0..n).filter(|&k| plan.slots[k].gravity == *g).collect()).collect();
            for group in &groups {
                for pair in group.windows(2) {
                    add(pos(pair[1]) - end(pair[0]) - gap(pair[0]), Eq, required);
                }
            }
            let present: Vec<&Vec<usize>> = groups.iter().filter(|g| !g.is_empty()).collect();
            for pair in present.windows(2) {
                let (a, b) = (pair[0][pair[0].len() - 1], pair[1][0]);
                add(pos(b) - end(a) - plan.spacing, Ge, required);
            }
            let (a, b) = (present[0][0], present[present.len() - 1][present[present.len() - 1].len() - 1]);
            add(pos(a) - start_inset, Ge, clip);
            add(end(b) - size + end_inset, Le, clip);
            if let Some(&a) = groups[0].first() {
                add(pos(a) - start_inset, Eq, required);
            }
            if let Some(&b) = groups[2].last() {
                add(end(b) - size + end_inset, Eq, required);
            }
            if let (Some(&a), Some(&b)) = (groups[1].first(), groups[1].last()) {
                add(pos(a) + end(b) - size, Eq, Weight::Priority(999.0));
            }
        }
    }

    // Across the stack.
    let fb_max = plan.slots.iter().map(|s| s.baselines.0).fold(0.0, f64::max);
    let lb_max = plan.slots.iter().map(|s| s.baselines.1).fold(0.0, f64::max);
    for (k, x) in v.iter().enumerate() {
        let (cpos, csize) = (x[2], x[3]);
        let aligned = match (plan.alignment, plan.vertical) {
            (NSLayoutAttribute::Top, false) | (NSLayoutAttribute::Leading | NSLayoutAttribute::Left, true) => {
                cpos - cross_start
            }
            (NSLayoutAttribute::Bottom, false) | (NSLayoutAttribute::Trailing | NSLayoutAttribute::Right, true) => {
                cpos + csize - cross
            }
            (NSLayoutAttribute::FirstBaseline, false) => cpos - cross_start - (fb_max - plan.slots[k].baselines.0),
            (NSLayoutAttribute::LastBaseline, false) => {
                cpos + csize - cross + i.bottom + (lb_max - plan.slots[k].baselines.1)
            }
            _ => cpos + 0.5 * csize - 0.5 * cross,
        };
        add(aligned, Eq, required);
        add(Expression::from(cpos), Ge, clip_cross);
        add(cpos + csize - cross, Le, clip_cross);
    }
    out
}

#[cfg(test)]
mod tests {
    use kasuari::{Constraint, Solver, Strength};

    use super::*;

    #[test]
    fn stacks_hug_just_below_views() {
        assert!(default_hugging() < 250.0 && default_hugging() > 249.99);
        assert_eq!(tie_break(1000), Weight::Tie(0.1));
    }

    #[test]
    fn an_empty_stack_shrinks() {
        let plan = Plan {
            vertical: false,
            alignment: NSLayoutAttribute::CenterY,
            distribution: NSStackViewDistribution::Fill,
            spacing: 8.0,
            insets: NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 },
            hugging: [default_hugging(); 2],
            clipping: [1000.0; 2],
            slots: Vec::new(),
        };
        let stack = [Variable::new(), Variable::new(), Variable::new(), Variable::new()];
        let mut solver = Solver::new();
        solver.add_constraint(Constraint::new(stack[2] - 50.0, RelationalOperator::Equal, Strength::new(1.0))).unwrap();
        for spec in build(&plan, stack, &[]) {
            let strength = match spec.weight {
                Weight::Priority(p) if p >= 1000.0 => Strength::REQUIRED,
                _ => Strength::new(1e6),
            };
            solver.add_constraint(Constraint::new(spec.expr, spec.op, strength)).unwrap();
        }
        assert_eq!(solver.get_value(stack[2]), 0.0);
    }
}

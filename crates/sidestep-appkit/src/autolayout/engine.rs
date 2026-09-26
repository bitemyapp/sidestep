//! The solver behind a tree of views: kasuari's Cassowary, with four
//! variables per item.
//!
//! **Variables.** Each view or guide taking part has `x`, `y`, `w` and `h`:
//! its alignment rectangle (the frame less `alignmentRectInsets`), placed
//! relative to its parent's frame origin, with `y` measured down from the
//! parent's top whether the parent is flipped or not. The parent is the
//! superview, or a guide's owning view. An anchor in the coordinates of an
//! ancestor is then a sum along the path: an item's `x`, plus each
//! intermediate view's frame origin (its `x` less its left inset). A
//! constraint is written in the coordinates of the view it's installed on,
//! which holds all of its items.
//!
//! **Who takes part.** The items of active constraints and every view
//! above them, up to the root of the tree (a window's content view, or the
//! top of a tree outside a window). A window's root holds its size at
//! `NSLayoutPriorityWindowSizeStayPut` (500), so stronger constraints
//! resize the window, as on macOS; a root outside a window that translates
//! its autoresizing mask keeps its frame's size (required), and one that
//! doesn't is sized by its constraints. A view that translates its mask
//! gets required constraints that move and size it as autoresizing would,
//! in proportion to its parent's size; a view that doesn't gets its
//! intrinsic content size, held by its hugging and compression resistance
//! priorities.
//!
//! **Priorities** are tiers. AppKit satisfies priorities strongest first,
//! so any number of constraints at 250 give way to one at 251. Cassowary
//! weighs errors by strength instead, so the engine gives each optional
//! priority it has seen a rank, and the ranks strengths spread evenly over
//! seven powers of ten (`Tiers`): with `k` priorities in use, each
//! outweighs the one below it `10^(7/k)` times, whatever their values.
//! That is lexicographic for the handful of priorities a window uses,
//! though not for any number. Priorities stay ranked once seen, so views
//! coming and going don't reweigh the solver; a new one reweighs the
//! optional constraints once, before the next solve (`retier`). 1000 is
//! required.
//!
//! **Keeping up.** Adding and removing constraints, and changes to their
//! constants and priorities, update the solver as they happen. A subtree
//! leaving the tree takes its items and the constraints installed in it
//! out of the solver (`forget_subtree`); one joining puts its own in
//! (`learn_subtree`); so a move costs what the moved subtree holds, not
//! what the window does.

use std::collections::HashMap;

use kasuari::{
    AddConstraintError, Constraint, Expression, RelationalOperator, Solver, Strength, Term as KTerm, Variable,
};
use objc2::msg_send;
use objc2::sel;
use objc2_app_kit::{NSAutoresizingMaskOptions, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation};
use objc2_foundation::{NSEdgeInsets, NSObjectProtocol, NSPoint, NSRect, NSSize};

use super::{ItemRef, Term};
use crate::view_layout;
use crate::views::{self, NSViewImpl};

const X: usize = 0;
const Y: usize = 1;
const W: usize = 2;
const H: usize = 3;

/// No intrinsic size along an axis.
pub(crate) const NO_INTRINSIC: f64 = -1.0;

/// Content hugging and compression resistance, as macOS gives a view.
pub(crate) const DEFAULT_HUGGING: f32 = 250.0;
pub(crate) const DEFAULT_COMPRESSION: f32 = 750.0;
/// How hard `fittingSize` pulls a view's size down.
const FITTING_COMPRESSION: f32 = 50.0;
/// How hard a window holds its content size: weaker constraints give way
/// to it, stronger ones resize the window.
const WINDOW_SIZE_STAY_PUT: f32 = 500.0;
/// A required constraint the others make impossible drops to this.
const BROKEN: f32 = 999.0;

/// How hard a constraint pulls.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Weight {
    /// A layout priority: 1000 is required, anything less a tier.
    Priority(f32),
    /// Weaker than any priority, by this much: a tie-break between equals.
    Tie(f64),
}

pub(crate) const REQUIRED: Weight = Weight::Priority(1000.0);

/// A constraint to make: `expr op 0` at `weight`.
pub(crate) struct Spec {
    pub expr: Expression,
    pub op: RelationalOperator,
    pub weight: Weight,
}

impl Spec {
    pub(crate) fn new(expr: impl Into<Expression>, op: RelationalOperator, weight: Weight) -> Spec {
        Spec { expr: expr.into(), op, weight }
    }
}

/// How many powers of ten the tiers span, from 1: kasuari's tolerances are
/// absolute, and it loses optima (at random, as its tables are hashed)
/// once optional strengths reach 10^8 alongside small ones.
const TIER_DECADES: f64 = 7.0;

/// The optional priorities an engine has seen, lowest first; see the
/// module documentation.
#[derive(Default)]
struct Tiers {
    list: Vec<f32>,
    /// Ranks moved since the optional constraints were last weighed.
    changed: bool,
}

impl Tiers {
    fn note(&mut self, weight: Weight) {
        if let Weight::Priority(p) = weight
            && p < 1000.0
            && let Err(at) = self.list.binary_search_by(|x| x.total_cmp(&p.max(0.0)))
        {
            self.list.insert(at, p.max(0.0));
            self.changed = true;
        }
    }

    fn strength(&self, weight: Weight) -> Strength {
        match weight {
            Weight::Priority(p) if p >= 1000.0 => Strength::REQUIRED,
            Weight::Priority(p) => {
                let rank = self.list.partition_point(|x| *x < p.max(0.0)) + 1;
                Strength::new(10f64.powf(TIER_DECADES * rank as f64 / self.list.len().max(1) as f64))
            }
            Weight::Tie(t) => Strength::new(t),
        }
    }
}

/// Weaker than any priority: what a probe for ambiguity pulls with.
const PROBE: Strength = Strength::new(1e-3);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A tree's engine, kept between passes.
    Tree,
    /// A view's own constraints only, its size pulled down (`fittingSize`).
    Fitting,
}

/// A constraint in the solver, with what it was made from, so it can be
/// weighed again.
struct Placed {
    constraint: Constraint,
    weight: Weight,
    strength: Strength,
}

struct Item {
    item: ItemRef,
    parent: Option<usize>,
    vars: [Variable; 4],
    insets: NSEdgeInsets,
    /// No view is narrower or shorter than nothing: made once.
    floor: Vec<Placed>,
    /// Constraints the engine makes for the item itself: the root's size,
    /// an autoresizing mask, an intrinsic size, or a stack's arrangement.
    generated: Vec<Placed>,
}

pub(crate) struct Engine {
    solver: Solver,
    items: HashMap<usize, Item>,
    owners: HashMap<Variable, usize>,
    constraints: HashMap<usize, Placed>,
    tiers: Tiers,
    root: usize,
    mode: Mode,
    /// The engine must be rebuilt from the tree before it's used.
    pub stale: bool,
    /// Something changed since the last solve.
    pub dirty: bool,
    /// The content size last asked of a window whose content needs another.
    pub asked: Option<NSSize>,
}

fn zero_insets() -> NSEdgeInsets {
    NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }
}

impl Engine {
    pub(crate) fn new(root: &NSViewImpl, mode: Mode) -> Engine {
        Engine {
            solver: Solver::new(),
            items: HashMap::new(),
            owners: HashMap::new(),
            constraints: HashMap::new(),
            tiers: Tiers::default(),
            root: ItemRef::view(root).key(),
            mode,
            stale: true,
            dirty: true,
            asked: None,
        }
    }

    /// Start again from the constraints installed in the tree at `root`.
    pub(crate) fn rebuild(&mut self, root: &NSViewImpl) {
        self.solver.reset();
        self.items.clear();
        self.owners.clear();
        self.constraints.clear();
        self.tiers = Tiers::default();
        self.root = ItemRef::view(root).key();
        self.stale = false;
        self.dirty = true;
        self.ensure(ItemRef::view(root));
        self.learn_subtree(root);
    }

    pub(crate) fn contains(&self, item: ItemRef) -> bool {
        self.items.contains_key(&item.key())
    }

    /// Put `item` in the engine, or make its own constraints again if it
    /// is in already.
    pub(crate) fn include(&mut self, item: ItemRef) {
        if self.contains(item) {
            self.regenerate(item);
        } else {
            self.ensure(item);
        }
    }

    /// The constraints installed in the subtree at `view`, which just
    /// joined the tree, and its stacks' arrangements.
    pub(crate) fn learn_subtree(&mut self, view: &NSViewImpl) {
        let mut installed = Vec::new();
        let mut stacks = Vec::new();
        collect(view, &mut installed, &mut stacks);
        for c in installed {
            self.add(&c);
        }
        for stack in stacks {
            self.ensure(ItemRef::view(views::imp(&stack)));
        }
        self.dirty = true;
    }

    /// Take the subtree at `view`, which is leaving the tree, out: its
    /// items and the constraints installed in it. Constraints between it
    /// and the rest are the caller's to deactivate first.
    pub(crate) fn forget_subtree(&mut self, view: &NSViewImpl) {
        if !view_layout::has_auto(view) {
            return;
        }
        for c in super::installed_on(view) {
            self.remove(&c);
        }
        for guide in super::guides_of(view) {
            self.forget_item(ItemRef::guide(&guide));
        }
        self.forget_item(ItemRef::view(view));
        for sub in views::subviews(view) {
            self.forget_subtree(views::imp(&sub));
        }
    }

    /// Take an item out: its variables and its own constraints.
    pub(crate) fn forget_item(&mut self, item: ItemRef) {
        let Some(gone) = self.items.remove(&item.key()) else { return };
        for p in gone.floor.iter().chain(&gone.generated) {
            let _ = self.solver.remove_constraint(&p.constraint);
        }
        for v in gone.vars {
            self.owners.remove(&v);
        }
        self.dirty = true;
    }

    /// Give `item` variables, and its parents theirs, if it hasn't any.
    /// None if it isn't in this engine's tree.
    fn ensure(&mut self, item: ItemRef) -> Option<usize> {
        let key = item.key();
        if self.items.contains_key(&key) {
            return Some(key);
        }
        let parent = if key == self.root {
            None
        } else {
            // SAFETY: items are alive: the caller holds them, or they are
            // the parents of items that are.
            let parent = unsafe { item.parent() }?;
            Some(self.ensure(ItemRef::view(parent))?)
        };
        let vars = [Variable::new(), Variable::new(), Variable::new(), Variable::new()];
        for v in vars {
            self.owners.insert(v, key);
        }
        // SAFETY: as above.
        let view = unsafe { item.as_view() };
        let insets = match view {
            Some(view) => views::as_view(view).alignmentRectInsets(),
            None => zero_insets(),
        };
        if let Some(view) = view {
            view_layout::set_engine_hint(view);
        }
        let ge = RelationalOperator::GreaterOrEqual;
        let floor =
            vec![self.put(Spec::new(vars[W], ge, REQUIRED), false), self.put(Spec::new(vars[H], ge, REQUIRED), false)];
        self.items.insert(key, Item { item, parent, vars, insets, floor, generated: Vec::new() });
        self.generate(key);
        Some(key)
    }

    fn var(&self, key: usize, which: usize) -> Variable {
        self.items[&key].vars[which]
    }

    /// Put a constraint in the solver at its weight's strength. A required
    /// one the others make impossible goes in just below required instead,
    /// and says so, as AppKit breaks one.
    fn put(&mut self, spec: Spec, breakable: bool) -> Placed {
        self.tiers.note(spec.weight);
        let strength = self.tiers.strength(spec.weight);
        let constraint = Constraint::new(spec.expr, spec.op, strength);
        match self.solver.add_constraint(constraint.clone()) {
            Err(AddConstraintError::UnsatisfiableConstraint) if breakable => {
                eprintln!(
                    "sidestep: unable to satisfy all required layout constraints; breaking one ({} {} 0)",
                    describe(constraint.expr()),
                    constraint.op()
                );
                let spec =
                    Spec { expr: constraint.expr().clone(), op: constraint.op(), weight: Weight::Priority(BROKEN) };
                self.put(spec, false)
            }
            _ => Placed { constraint, weight: spec.weight, strength },
        }
    }

    /// Put the item's own constraints (root size, mask, intrinsic size or
    /// a stack's arrangement) in the solver, replacing any it had.
    fn generate(&mut self, key: usize) {
        let old = std::mem::take(&mut self.items.get_mut(&key).expect("an item").generated);
        for p in &old {
            let _ = self.solver.remove_constraint(&p.constraint);
        }
        self.dirty = true;
        let item = self.items[&key].item;
        // SAFETY: items are alive.
        let Some(view) = (unsafe { item.as_view() }) else { return };
        let vars = self.items[&key].vars;
        let insets = self.items[&key].insets;
        let frame = views::frame(view);
        let (fw, fh) = (frame.size.width - insets.left - insets.right, frame.size.height - insets.top - insets.bottom);
        let translates = view_layout::translates_mask(view);
        let mut new = Vec::new();
        let eq = |v: Variable, value: f64, w: Weight| Spec::new(v - value, RelationalOperator::Equal, w);
        if key == self.root {
            match self.mode {
                Mode::Fitting => {
                    new.push(eq(vars[W], 0.0, Weight::Priority(FITTING_COMPRESSION)));
                    new.push(eq(vars[H], 0.0, Weight::Priority(FITTING_COMPRESSION)));
                    new.extend(intrinsic(view, vars));
                }
                Mode::Tree if views::window_of(view).is_some() => {
                    new.push(eq(vars[W], fw, Weight::Priority(WINDOW_SIZE_STAY_PUT)));
                    new.push(eq(vars[H], fh, Weight::Priority(WINDOW_SIZE_STAY_PUT)));
                }
                Mode::Tree if translates => {
                    new.push(eq(vars[W], fw, REQUIRED));
                    new.push(eq(vars[H], fh, REQUIRED));
                }
                Mode::Tree => {
                    // Sized by its constraints; where they leave it free, it
                    // keeps its size.
                    new.push(eq(vars[W], fw, Weight::Priority(1.0)));
                    new.push(eq(vars[H], fh, Weight::Priority(1.0)));
                    new.extend(intrinsic(view, vars));
                }
            }
        } else if translates {
            let parent = self.items[&key].parent.expect("a parent");
            let p = &self.items[&parent];
            // SAFETY: items are alive.
            let parent_view = unsafe { p.item.as_view() }.expect("a view's parent is a view");
            new.extend(mask(view, frame, insets, parent_view, p.vars, p.insets, vars));
        } else {
            new.extend(intrinsic(view, vars));
        }
        if let Some(stack) = super::stack::as_stack(view) {
            // A stack's constraints on its arranged views.
            let plan = super::stack::plan(stack);
            let mut kids = Vec::with_capacity(plan.slots.len());
            for slot in &plan.slots {
                match self.ensure(ItemRef::view(views::imp(&slot.view))) {
                    Some(k) => kids.push(self.items[&k].vars),
                    None => return,
                }
            }
            new.extend(super::stack::build(&plan, vars, &kids));
        }
        let placed: Vec<Placed> = new.into_iter().map(|spec| self.put(spec, true)).collect();
        // `ensure` above may have made items, but not this one again.
        self.items.get_mut(&key).expect("an item").generated = placed;
    }

    /// The item's frame or its size or priorities changed: make its own
    /// constraints again.
    pub(crate) fn regenerate(&mut self, item: ItemRef) {
        if self.items.contains_key(&item.key()) {
            self.generate(item.key());
        }
    }

    /// Put an active constraint in the solver. Nothing happens if one of
    /// its items is gone or outside this engine's tree.
    pub(crate) fn add(&mut self, c: &NSLayoutConstraint) {
        let Some(top) = super::constraint::installed(c) else { return };
        // SAFETY: the view it's installed on holds it, and clears the link
        // when it goes.
        let top = ItemRef::view(views::imp(unsafe { top.as_ref() }));
        // `parts` holds the items while their terms are read.
        let Some(parts) = super::constraint::parts(c) else { return };
        let Some(top) = self.ensure(top) else { return };
        let mut expr = Expression::from_constant(-parts.constant);
        if !self.side(&parts.first, top, 1.0, &mut expr) || !self.side(&parts.second, top, -parts.multiplier, &mut expr)
        {
            return;
        }
        let op = match parts.relation {
            NSLayoutRelation::LessThanOrEqual => RelationalOperator::LessOrEqual,
            NSLayoutRelation::GreaterThanOrEqual => RelationalOperator::GreaterOrEqual,
            _ => RelationalOperator::Equal,
        };
        let placed = self.put(Spec { expr, op, weight: Weight::Priority(parts.priority) }, true);
        if let Some(old) = self.constraints.insert(key_of(c), placed) {
            let _ = self.solver.remove_constraint(&old.constraint);
        }
        self.dirty = true;
    }

    /// Take a constraint out of the solver.
    pub(crate) fn remove(&mut self, c: &NSLayoutConstraint) {
        if let Some(placed) = self.constraints.remove(&key_of(c)) {
            let _ = self.solver.remove_constraint(&placed.constraint);
            self.dirty = true;
        }
    }

    /// Weigh the optional constraints again after a new priority moved the
    /// ranks.
    fn retier(&mut self) {
        if !self.tiers.changed {
            return;
        }
        self.tiers.changed = false;
        let tiers = &self.tiers;
        let solver = &mut self.solver;
        let all = self.constraints.values_mut().chain(self.items.values_mut().flat_map(|i| i.generated.iter_mut()));
        for p in all {
            let strength = tiers.strength(p.weight);
            if strength == p.strength {
                continue;
            }
            let _ = solver.remove_constraint(&p.constraint);
            let c = Constraint::new(p.constraint.expr().clone(), p.constraint.op(), strength);
            let _ = solver.add_constraint(c.clone());
            *p = Placed { constraint: c, weight: p.weight, strength };
        }
        self.dirty = true;
    }

    /// Add `coeff` times a side's terms, in the coordinates of `top`.
    fn side(&mut self, terms: &[Term], top: usize, coeff: f64, expr: &mut Expression) -> bool {
        for t in terms {
            let Some(key) = self.ensure(t.item) else { return false };
            self.attribute(key, t.attr, top, coeff * t.coeff, expr);
        }
        true
    }

    fn attribute(&self, key: usize, attr: NSLayoutAttribute, top: usize, c: f64, expr: &mut Expression) {
        let (x, y, w, h) = (X, Y, W, H);
        let size =
            |expr: &mut Expression, which: usize, k: f64| expr.terms.push(KTerm::new(self.var(key, which), c * k));
        match attr {
            NSLayoutAttribute::Width => size(expr, w, 1.0),
            NSLayoutAttribute::Height => size(expr, h, 1.0),
            NSLayoutAttribute::Left | NSLayoutAttribute::Leading => self.position(key, top, x, c, expr),
            NSLayoutAttribute::Right | NSLayoutAttribute::Trailing => {
                self.position(key, top, x, c, expr);
                size(expr, w, 1.0);
            }
            NSLayoutAttribute::CenterX => {
                self.position(key, top, x, c, expr);
                size(expr, w, 0.5);
            }
            NSLayoutAttribute::Top => self.position(key, top, y, c, expr),
            NSLayoutAttribute::Bottom => {
                self.position(key, top, y, c, expr);
                size(expr, h, 1.0);
            }
            NSLayoutAttribute::CenterY => {
                self.position(key, top, y, c, expr);
                size(expr, h, 0.5);
            }
            NSLayoutAttribute::FirstBaseline => {
                self.position(key, top, y, c, expr);
                expr.constant += c * self.baseline(key, sel!(firstBaselineOffsetFromTop));
            }
            NSLayoutAttribute::LastBaseline => {
                self.position(key, top, y, c, expr);
                size(expr, h, 1.0);
                expr.constant -= c * self.baseline(key, sel!(lastBaselineOffsetFromBottom));
            }
            _ => {}
        }
    }

    /// An item's leading or top edge in `top`'s coordinates, times `c`.
    fn position(&self, key: usize, top: usize, which: usize, c: f64, expr: &mut Expression) {
        let start = |i: &Item| if which == X { i.insets.left } else { i.insets.top };
        if key == top {
            expr.constant += c * start(&self.items[&key]);
            return;
        }
        expr.terms.push(KTerm::new(self.var(key, which), c));
        let mut cur = self.items[&key].parent;
        while let Some(p) = cur {
            if p == top {
                return;
            }
            let item = &self.items[&p];
            expr.terms.push(KTerm::new(item.vars[which], c));
            expr.constant -= c * start(item);
            cur = item.parent;
        }
    }

    fn baseline(&self, key: usize, selector: objc2::runtime::Sel) -> f64 {
        // SAFETY: items are alive.
        match unsafe { self.items[&key].item.as_view() } {
            Some(view) if views::as_view(view).respondsToSelector(selector) => {
                // SAFETY: both baseline offsets take nothing and return a
                // CGFloat.
                unsafe { objc2::runtime::MessageReceiver::send_message::<(), f64>(views::as_view(view), selector, ()) }
            }
            _ => 0.0,
        }
    }

    /// Bring the solution up to date: weigh constraints again if a new
    /// priority came in.
    pub(crate) fn settle(&mut self) {
        self.retier();
    }

    /// The items whose variables changed since the last call, by key.
    pub(crate) fn changes(&mut self) -> Vec<ItemRef> {
        self.retier();
        let mut keys: Vec<usize> =
            self.solver.fetch_changes().iter().filter_map(|(v, _)| self.owners.get(v).copied()).collect();
        keys.sort_unstable();
        keys.dedup();
        self.dirty = false;
        keys.into_iter().filter_map(|k| self.items.get(&k).map(|i| i.item)).collect()
    }

    /// The item's frame in its parent's coordinates, as solved.
    pub(crate) fn frame(&self, item: ItemRef) -> Option<NSRect> {
        let i = self.items.get(&item.key())?;
        let v = |which| self.solver.get_value(i.vars[which]);
        let ins = i.insets;
        let x = v(X) - ins.left;
        let w = v(W) + ins.left + ins.right;
        let h = v(H) + ins.top + ins.bottom;
        let top = v(Y) - ins.top;
        // SAFETY: items are alive.
        let y = match unsafe { item.parent() } {
            Some(parent) if !views::is_flipped(parent) => views::frame(parent).size.height - top - h,
            _ => top,
        };
        Some(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)))
    }

    /// The root's size, as solved.
    pub(crate) fn root_size(&self) -> NSSize {
        let i = &self.items[&self.root];
        let ins = i.insets;
        NSSize::new(
            self.solver.get_value(i.vars[W]) + ins.left + ins.right,
            self.solver.get_value(i.vars[H]) + ins.top + ins.bottom,
        )
    }

    /// Whether some variable of the item is free: a pull weaker than any
    /// constraint moves it.
    pub(crate) fn is_ambiguous(&mut self, item: ItemRef) -> bool {
        self.retier();
        let Some(i) = self.items.get(&item.key()) else { return false };
        let vars = i.vars;
        vars.into_iter().any(|v| {
            let before = self.solver.get_value(v);
            if self.solver.add_edit_variable(v, PROBE).is_err() {
                return false;
            }
            let _ = self.solver.suggest_value(v, before + 97.0);
            let moved = (self.solver.get_value(v) - before).abs() > 0.5;
            let _ = self.solver.suggest_value(v, before);
            let _ = self.solver.remove_edit_variable(v);
            moved
        })
    }

    /// Whether the solver holds `c`.
    pub(crate) fn has_constraint(&self, c: &NSLayoutConstraint) -> bool {
        self.constraints.contains_key(&key_of(c))
    }
}

fn key_of(c: &NSLayoutConstraint) -> usize {
    (c as *const NSLayoutConstraint).addr()
}

fn describe(e: &Expression) -> String {
    let mut s = String::new();
    for t in &e.terms {
        s.push_str(&format!("{:+}·v{:?} ", t.coefficient, t.variable));
    }
    s.push_str(&format!("{:+}", e.constant));
    s
}

/// Every active constraint installed in the tree at `view`, and every
/// stack view arranging views: only subtrees that took part in Auto Layout
/// can hold them.
fn collect(
    view: &NSViewImpl,
    out: &mut Vec<objc2::rc::Retained<NSLayoutConstraint>>,
    stacks: &mut Vec<objc2::rc::Retained<objc2_app_kit::NSView>>,
) {
    if !view_layout::has_auto(view) {
        return;
    }
    out.extend(super::installed_on(view));
    if super::stack::arranges(view) {
        stacks.push(objc2::Message::retain(views::as_view(view)));
    }
    for sub in views::subviews(view) {
        collect(views::imp(&sub), out, stacks);
    }
}

/// Constraints for an intrinsic content size, held by the view's hugging
/// and compression resistance.
fn intrinsic(view: &NSViewImpl, vars: [Variable; 4]) -> Vec<Spec> {
    let v = views::as_view(view);
    if !v.respondsToSelector(sel!(intrinsicContentSize)) {
        return Vec::new();
    }
    // SAFETY: intrinsicContentSize takes nothing and returns an NSSize.
    let size: NSSize = unsafe { msg_send![v, intrinsicContentSize] };
    let [hug_h, hug_v, comp_h, comp_v] = super::priorities(view);
    let mut out = Vec::new();
    for (value, var, hug, comp) in [(size.width, vars[W], hug_h, comp_h), (size.height, vars[H], hug_v, comp_v)] {
        if value == NO_INTRINSIC {
            continue;
        }
        out.push(Spec::new(var - value, RelationalOperator::LessOrEqual, Weight::Priority(hug)));
        out.push(Spec::new(var - value, RelationalOperator::GreaterOrEqual, Weight::Priority(comp)));
    }
    out
}

/// How much of a change in the parent's size a view's minimum margin,
/// size and maximum margin take, as autoresizing shares it (in proportion
/// to their extents among the flexible ones).
pub(crate) fn shares(pos: f64, size: f64, max_margin: f64, flexible: [bool; 3]) -> [f64; 3] {
    let extents = [pos, size, max_margin];
    let count = flexible.iter().filter(|f| **f).count();
    let total: f64 = (0..3).filter(|&i| flexible[i]).map(|i| extents[i].max(0.0)).sum();
    let mut out = [0.0; 3];
    for i in 0..3 {
        if flexible[i] {
            out[i] = if total > 0.0 { extents[i].max(0.0) / total } else { 1.0 / count as f64 };
        }
    }
    out
}

/// Required constraints reproducing a view's autoresizing mask: its frame
/// as autoresizing would place it for any size of its parent.
fn mask(
    view: &NSViewImpl,
    frame: NSRect,
    insets: NSEdgeInsets,
    parent: &NSViewImpl,
    parent_vars: [Variable; 4],
    parent_insets: NSEdgeInsets,
    vars: [Variable; 4],
) -> Vec<Spec> {
    let m: NSAutoresizingMaskOptions = views::as_view(view).autoresizingMask();
    let resizes = view_layout::autoresizes_subviews(parent);
    let has = |o: NSAutoresizingMaskOptions| resizes && m.contains(o);
    let parent_size = views::frame(parent).size;
    let (x0, y0, w0, h0) = (frame.origin.x, frame.origin.y, frame.size.width, frame.size.height);
    let (pw, ph) = (parent_size.width, parent_size.height);
    let [kx, kw, _] = shares(
        x0,
        w0,
        pw - x0 - w0,
        [
            has(NSAutoresizingMaskOptions::ViewMinXMargin),
            has(NSAutoresizingMaskOptions::ViewWidthSizable),
            has(NSAutoresizingMaskOptions::ViewMaxXMargin),
        ],
    );
    let [ky, kh, _] = shares(
        y0,
        h0,
        ph - y0 - h0,
        [
            has(NSAutoresizingMaskOptions::ViewMinYMargin),
            has(NSAutoresizingMaskOptions::ViewHeightSizable),
            has(NSAutoresizingMaskOptions::ViewMaxYMargin),
        ],
    );
    // The parent's frame size is its variable plus its insets.
    let pdw = parent_insets.left + parent_insets.right;
    let pdh = parent_insets.top + parent_insets.bottom;
    let eq = |e: Expression| Spec::new(e, RelationalOperator::Equal, REQUIRED);
    // frame.x = x0 + kx·(PW − pw), with PW = parent.w + pdw.
    let x = eq(vars[X] - kx * parent_vars[W] - (x0 + insets.left + kx * (pdw - pw)));
    let w = eq(vars[W] - kw * parent_vars[W] - (w0 - insets.left - insets.right + kw * (pdw - pw)));
    let h = eq(vars[H] - kh * parent_vars[H] - (h0 - insets.top - insets.bottom + kh * (pdh - ph)));
    let y = if views::is_flipped(parent) {
        eq(vars[Y] - ky * parent_vars[H] - (y0 + insets.top + ky * (pdh - ph)))
    } else {
        // Measured from the top: PH − frame.y − frame.h.
        let k = 1.0 - ky - kh;
        eq(vars[Y] - k * parent_vars[H] - (k * pdh - y0 - h0 + (ky + kh) * ph + insets.top))
    };
    vec![x, y, w, h]
}

/// Round a frame's edges to the backing store's pixels.
pub(crate) fn round_frame(r: NSRect, scale: f64) -> NSRect {
    let round = |v: f64| (v * scale).round() / scale;
    let (x0, y0) = (round(r.origin.x), round(r.origin.y));
    let (x1, y1) = (round(r.origin.x + r.size.width), round(r.origin.y + r.size.height));
    NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_are_tiers() {
        let mut t = Tiers::default();
        for p in [250.0, 251.0, 750.0, 250.0, 1000.0] {
            t.note(Weight::Priority(p));
        }
        assert_eq!(t.list, [250.0, 251.0, 750.0]);
        let s = |p| t.strength(Weight::Priority(p)).value();
        assert_eq!(t.strength(REQUIRED), Strength::REQUIRED);
        assert!(s(750.0) <= 1e7);
        // Each tier outweighs many of the one below, however close.
        assert!(s(251.0) > 100.0 * s(250.0));
        assert!(s(750.0) > 100.0 * s(251.0));
        // Ties and probes are weaker than any tier.
        assert!(t.strength(Weight::Tie(0.1)).value() < s(250.0) && PROBE.value() < s(250.0));
        // A new priority moves the ranks.
        t.changed = false;
        t.note(Weight::Priority(249.5));
        let s = |p| t.strength(Weight::Priority(p)).value();
        assert!(t.changed && s(249.5) < s(250.0));
    }

    #[test]
    fn mask_shares_follow_autoresizing() {
        // One flexible part takes the whole change.
        assert_eq!(shares(10.0, 50.0, 140.0, [false, true, false]), [0.0, 1.0, 0.0]);
        // Several share it in proportion to their extents.
        assert_eq!(shares(10.0, 50.0, 140.0, [true, true, true]), [0.05, 0.25, 0.7]);
        // All empty: equal parts.
        assert_eq!(shares(0.0, 0.0, 0.0, [true, true, false]), [0.5, 0.5, 0.0]);
        assert_eq!(shares(10.0, 50.0, 140.0, [false, false, false]), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn frames_round_by_their_edges() {
        let r = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        let third = 100.0 / 3.0;
        assert_eq!(round_frame(r(third, 0.0, third, 10.3), 1.0), r(33.0, 0.0, 34.0, 10.0));
        assert_eq!(round_frame(r(third, 89.7, third, 10.3), 2.0), r(33.5, 89.5, 33.0, 10.5));
    }
}

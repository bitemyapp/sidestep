//! `CGEvent` as a value: events a program makes and hands to AppKit
//! (`+[NSEvent eventWithCGEvent:]`) or reads back out of one
//! (`-[NSEvent CGEvent]`), as tests synthesizing scrolls and clicks do.
//! Posting events (`CGEventPost`), taps and event sources belong to the
//! window server and aren't here; a source argument is ignored.
//!
//! An event is its type, location (global, top-left origin), flags,
//! timestamp (nanoseconds on `NSEvent`'s clock) and its numbered fields,
//! each kept as it was set: integer or double, read back as either (a
//! scroll axis's whole and fixed-point line deltas are one, set together).
//! Scroll-wheel events made by `CGEventCreateScrollWheelEvent2` fill in the
//! fields of their unit: pixels make a continuous event with point deltas,
//! lines an event with line deltas.

use std::collections::BTreeMap;
use std::ptr::NonNull;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFTypeID, CGPoint};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags, CGEventType, CGMouseButton, CGScrollEventUnit};
use objc2_foundation::NSString;

/// A field's value, as it was set.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    Integer(i64),
    Double(f64),
}

/// What an event holds.
#[derive(Clone, Debug, Default)]
pub(crate) struct Fields {
    pub(crate) kind: u32,
    pub(crate) location: CGPoint,
    pub(crate) flags: u64,
    pub(crate) timestamp: u64,
    values: BTreeMap<u32, Value>,
}

impl Fields {
    pub(crate) fn integer(&self, field: CGEventField) -> i64 {
        match self.values.get(&field.0) {
            Some(Value::Integer(v)) => *v,
            Some(Value::Double(v)) => *v as i64,
            None => 0,
        }
    }

    pub(crate) fn double(&self, field: CGEventField) -> f64 {
        match self.values.get(&field.0) {
            Some(Value::Integer(v)) => *v as f64,
            Some(Value::Double(v)) => *v,
            None => 0.0,
        }
    }

    /// Whether `field` was set at all.
    pub(crate) fn has(&self, field: CGEventField) -> bool {
        self.values.contains_key(&field.0)
    }

    pub(crate) fn set_integer(&mut self, field: CGEventField, value: i64) {
        self.values.insert(field.0, Value::Integer(value));
    }

    pub(crate) fn set_double(&mut self, field: CGEventField, value: f64) {
        self.values.insert(field.0, Value::Double(value));
    }
}

pub(crate) struct EventIvars {
    fields: Mutex<Fields>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the fields are
    // behind a lock.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGEvent"]
    #[ivars = EventIvars]
    pub(crate) struct CGEventImpl;

    impl CGEventImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let f = self.fields();
            let detail = format!("type = {}, location = ({}, {}), flags = {:#x}", f.kind, f.location.x, f.location.y, f.flags);
            super::description("CGEvent", self, &detail)
        }
    }

    unsafe impl NSObjectProtocol for CGEventImpl {}
);

impl CGEventImpl {
    pub(crate) fn fields(&self) -> Fields {
        self.ivars().fields.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn update(&self, f: impl FnOnce(&mut Fields)) {
        f(&mut self.ivars().fields.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

/// A new event holding `fields`.
pub(crate) fn with_fields(fields: Fields) -> Retained<CGEventImpl> {
    let this = CGEventImpl::alloc().set_ivars(EventIvars { fields: Mutex::new(fields) });
    // SAFETY: NSObject's init.
    unsafe { msg_send![super(this), init] }
}

/// `event` as Sidestep's event class, if it is one.
pub(crate) fn of(event: &CGEvent) -> Option<&CGEventImpl> {
    // SAFETY: a CGEvent is an Objective-C object in Sidestep.
    let object = unsafe { &*(event as *const CGEvent).cast::<AnyObject>() };
    object.downcast_ref::<CGEventImpl>()
}

/// The event as the type objc2-core-graphics names.
pub(crate) fn as_cg(event: Retained<CGEventImpl>) -> Retained<CGEvent> {
    // SAFETY: CGEventImpl is the class of CGEvents.
    unsafe { Retained::cast_unchecked(event) }
}

fn get(event: Option<&CGEvent>) -> Option<&CGEventImpl> {
    event.and_then(of)
}

fn give(event: Retained<CGEventImpl>) -> Option<NonNull<CGEvent>> {
    NonNull::new(Retained::into_raw(event).cast())
}

/// Now, in nanoseconds on `NSEvent`'s clock (its timestamps are seconds).
fn now() -> u64 {
    (crate::event::uptime() * 1e9) as u64
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_EVENT
}

/// A null event (type 0), made now.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventCreate(_source: *const AnyObject) -> Option<NonNull<CGEvent>> {
    give(with_fields(Fields { timestamp: now(), ..Fields::default() }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventCreateCopy(event: Option<&CGEvent>) -> Option<NonNull<CGEvent>> {
    give(with_fields(get(event)?.fields()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventCreateMouseEvent(
    _source: *const AnyObject,
    kind: CGEventType,
    location: CGPoint,
    button: CGMouseButton,
) -> Option<NonNull<CGEvent>> {
    let mut fields = Fields { kind: kind.0, location, timestamp: now(), ..Fields::default() };
    fields.set_integer(CGEventField::MouseEventButtonNumber, button.0 as i64);
    let pressed = [CGEventType::LeftMouseDown, CGEventType::RightMouseDown, CGEventType::OtherMouseDown];
    let dragged = [CGEventType::LeftMouseDragged, CGEventType::RightMouseDragged, CGEventType::OtherMouseDragged];
    let up = [CGEventType::LeftMouseUp, CGEventType::RightMouseUp, CGEventType::OtherMouseUp];
    if pressed.contains(&kind) || up.contains(&kind) {
        fields.set_integer(CGEventField::MouseEventClickState, 1);
    }
    if pressed.contains(&kind) || dragged.contains(&kind) {
        fields.set_double(CGEventField::MouseEventPressure, 1.0);
    }
    give(with_fields(fields))
}

/// A scroll of up to three wheels (vertical, horizontal, depth) by
/// `wheel1`..`wheel3` in `units`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventCreateScrollWheelEvent2(
    _source: *const AnyObject,
    units: CGScrollEventUnit,
    wheel_count: u32,
    wheel1: i32,
    wheel2: i32,
    wheel3: i32,
) -> Option<NonNull<CGEvent>> {
    let mut fields = Fields { kind: CGEventType::ScrollWheel.0, timestamp: now(), ..Fields::default() };
    let axes = [
        (
            wheel1,
            CGEventField::ScrollWheelEventDeltaAxis1,
            CGEventField::ScrollWheelEventFixedPtDeltaAxis1,
            CGEventField::ScrollWheelEventPointDeltaAxis1,
        ),
        (
            wheel2,
            CGEventField::ScrollWheelEventDeltaAxis2,
            CGEventField::ScrollWheelEventFixedPtDeltaAxis2,
            CGEventField::ScrollWheelEventPointDeltaAxis2,
        ),
        (
            wheel3,
            CGEventField::ScrollWheelEventDeltaAxis3,
            CGEventField::ScrollWheelEventFixedPtDeltaAxis3,
            CGEventField::ScrollWheelEventPointDeltaAxis3,
        ),
    ];
    let pixels = units == CGScrollEventUnit::Pixel;
    for &(amount, lines, fixed, points) in axes.iter().take(wheel_count.clamp(1, 3) as usize) {
        let amount = amount as f64;
        // About a line per 10 points, as AppKit's coarse deltas count.
        let (in_lines, in_points) = if pixels { (amount / 10.0, amount) } else { (amount, amount * 10.0) };
        fields.set_integer(lines, in_lines.round() as i64);
        fields.set_double(fixed, in_lines);
        fields.set_double(points, in_points);
    }
    fields.set_integer(CGEventField::ScrollWheelEventIsContinuous, pixels as i64);
    give(with_fields(fields))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetType(event: Option<&CGEvent>) -> CGEventType {
    CGEventType(get(event).map_or(0, |e| e.fields().kind))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventSetType(event: Option<&CGEvent>, kind: CGEventType) {
    if let Some(e) = get(event) {
        e.update(|f| f.kind = kind.0);
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetTimestamp(event: Option<&CGEvent>) -> u64 {
    get(event).map_or(0, |e| e.fields().timestamp)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventSetTimestamp(event: Option<&CGEvent>, timestamp: u64) {
    if let Some(e) = get(event) {
        e.update(|f| f.timestamp = timestamp);
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetLocation(event: Option<&CGEvent>) -> CGPoint {
    get(event).map_or(CGPoint::ZERO, |e| e.fields().location)
}

/// The location with its origin at the bottom left of the first screen, as
/// AppKit's screen coordinates have it.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetUnflippedLocation(event: Option<&CGEvent>) -> CGPoint {
    let location = CGEventGetLocation(event);
    CGPoint::new(location.x, crate::screen::first_screen_height().map_or(location.y, |h| h - location.y))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventSetLocation(event: Option<&CGEvent>, location: CGPoint) {
    if let Some(e) = get(event) {
        e.update(|f| f.location = location);
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetFlags(event: Option<&CGEvent>) -> CGEventFlags {
    CGEventFlags(get(event).map_or(0, |e| e.fields().flags))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventSetFlags(event: Option<&CGEvent>, flags: CGEventFlags) {
    if let Some(e) = get(event) {
        e.update(|f| f.flags = flags.0);
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetIntegerValueField(event: Option<&CGEvent>, field: CGEventField) -> i64 {
    get(event).map_or(0, |e| e.fields().integer(field))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventSetIntegerValueField(event: Option<&CGEvent>, field: CGEventField, value: i64) {
    if let Some(e) = get(event) {
        e.update(|f| {
            f.set_integer(field, value);
            // A scroll's lines, as a whole number and in fixed point, are one
            // delta: setting one sets the other.
            if let Some(fixed) = fixed_point_of(field) {
                f.set_double(fixed, value as f64);
            }
        });
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventGetDoubleValueField(event: Option<&CGEvent>, field: CGEventField) -> f64 {
    get(event).map_or(0.0, |e| e.fields().double(field))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGEventSetDoubleValueField(event: Option<&CGEvent>, field: CGEventField, value: f64) {
    if let Some(e) = get(event) {
        e.update(|f| {
            f.set_double(field, value);
            if let Some(lines) = whole_lines_of(field) {
                f.set_integer(lines, value.round() as i64);
            }
        });
    }
}

/// A scroll axis's whole lines field and its fixed-point twin.
const LINE_FIELDS: [(CGEventField, CGEventField); 3] = [
    (CGEventField::ScrollWheelEventDeltaAxis1, CGEventField::ScrollWheelEventFixedPtDeltaAxis1),
    (CGEventField::ScrollWheelEventDeltaAxis2, CGEventField::ScrollWheelEventFixedPtDeltaAxis2),
    (CGEventField::ScrollWheelEventDeltaAxis3, CGEventField::ScrollWheelEventFixedPtDeltaAxis3),
];

fn fixed_point_of(field: CGEventField) -> Option<CGEventField> {
    LINE_FIELDS.iter().find(|(lines, _)| *lines == field).map(|(_, fixed)| *fixed)
}

fn whole_lines_of(field: CGEventField) -> Option<CGEventField> {
    LINE_FIELDS.iter().find(|(_, fixed)| *fixed == field).map(|(lines, _)| *lines)
}

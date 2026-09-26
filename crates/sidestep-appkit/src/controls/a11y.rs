//! Accessibility, store only: views and cells keep what programs set with
//! `setAccessibility…:` and answer it back, and until a program sets a
//! property they answer the default macOS gives their class
//! (`conformance/tests/controls.rs`, `accessibility`). Nothing reads the
//! store yet.
//!
//! It is shaped for an AccessKit adapter. Each object has one record,
//! keyed by its address and removed when it is deallocated (its [`Node`]
//! does that), whose fields are AccessKit's node properties: role, label,
//! description (`accessibilityHelp`), value, placeholder, author id
//! (`accessibilityIdentifier`), hidden and disabled. As on macOS, a control
//! isn't an element itself: its cell stands for it (a button's cell has the
//! button role and the title as its label), except for the cell-less
//! `NSSwitch` and views such as `NSBox` and `NSProgressIndicator`. So what
//! a program sets on a control reaches the cell as macOS has it reach
//! (`accessibility`): help set on a control is the cell's help too; a
//! control's label, until set, is its cell's; and a button whose label was
//! set gives its cell an empty label, so the element isn't announced twice.
//!
//! The methods are added to NSView and NSCell when their classes load,
//! one set of functions for both.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::CString;

use objc2::encode::{EncodeArguments, EncodeReturn};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, MethodImplementation, Sel};
use objc2::{Message, msg_send, sel};
use objc2_app_kit as ak;
use objc2_app_kit::{
    NSBox, NSBoxType, NSButton, NSButtonCell, NSCell, NSCellType, NSControl, NSProgressIndicator, NSSearchFieldCell,
    NSSecureTextFieldCell, NSSegmentedCell, NSSliderCell, NSStepperCell, NSSwitch, NSTextField, NSTextFieldCell,
    NSView,
};
use objc2_foundation::{NSNumber, NSString};

use super::button::{self, Look};

/// Unset (the class's default), or what a program set, nil included.
pub(crate) type Set<T> = Option<Option<Retained<T>>>;

/// What a program set on one object.
#[derive(Default)]
pub(crate) struct Props {
    pub role: Set<NSString>,
    pub subrole: Set<NSString>,
    pub role_description: Set<NSString>,
    pub label: Set<NSString>,
    pub title: Set<NSString>,
    pub help: Set<NSString>,
    pub identifier: Set<NSString>,
    pub placeholder: Set<NSString>,
    pub value: Set<AnyObject>,
    pub value_description: Set<NSString>,
    pub element: Option<bool>,
    pub hidden: Option<bool>,
    pub enabled: Option<bool>,
}

thread_local! {
    /// Records by object address, for objects that have any.
    static TABLE: RefCell<HashMap<usize, Props>> = RefCell::new(HashMap::new());
}

/// An object's hold on its record: the key, once it has one. Views and
/// cells keep one in their ivars, and dropping it removes the record, so a
/// later object at the same address starts afresh.
#[derive(Default)]
pub(crate) struct Node(Cell<usize>);

impl Drop for Node {
    fn drop(&mut self) {
        let key = self.0.get();
        if key != 0 {
            // The record drops after the borrow ends: its values may be
            // views, whose own nodes come back here.
            let removed = TABLE.try_with(|t| t.try_borrow_mut().ok().and_then(|mut t| t.remove(&key)));
            drop(removed);
        }
    }
}

fn node(this: &AnyObject) -> Option<&Node> {
    if let Some(view) = this.downcast_ref::<NSView>() {
        Some(crate::views::a11y_node(view))
    } else {
        this.downcast_ref::<NSCell>().map(super::cell::a11y_node)
    }
}

/// Read one field of `this`'s record, if it has one.
fn read<T>(this: &AnyObject, field: impl FnOnce(&Props) -> Option<T>) -> Option<T> {
    let key = this as *const AnyObject as usize;
    TABLE.with_borrow(|t| t.get(&key).and_then(field))
}

/// Change one field of `this`'s record, making the record if need be.
fn write<T>(this: &AnyObject, field: impl FnOnce(&mut Props) -> &mut T, value: T) {
    let Some(node) = node(this) else { return };
    let key = this as *const AnyObject as usize;
    node.0.set(key);
    let old = TABLE.with_borrow_mut(|t| std::mem::replace(field(t.entry(key).or_default()), value));
    // Released outside the borrow (see `Node`).
    drop(old);
}

fn string(s: &'static NSString) -> Option<Retained<NSString>> {
    Some(s.retain())
}

fn is<T: objc2::DowncastTarget>(this: &AnyObject) -> bool {
    this.downcast_ref::<T>().is_some()
}

/// The label until one is set: a control's is its cell's; a button
/// cell's is its title, or nothing once its button has a label of its
/// own; other objects have none.
fn default_label(this: &AnyObject) -> Option<Retained<NSString>> {
    if let Some(control) = this.downcast_ref::<NSControl>() {
        return match control.cell() {
            // SAFETY: accessibilityLabel takes nothing and returns a string
            // or nil (the cell's, as installed below).
            Some(cell) => unsafe { msg_send![&*cell, accessibilityLabel] },
            None => title_of(this),
        };
    }
    if let Some(cell) = this.downcast_ref::<NSButtonCell>() {
        // SAFETY: controlView takes nothing and returns a view or nil.
        let view = unsafe { cell.controlView() };
        if view.is_some_and(|v| read(&v, |p| p.label.as_ref().map(|_| ())).is_some()) {
            return Some(NSString::new());
        }
    }
    title_of(this)
}

fn title_of(this: &AnyObject) -> Option<Retained<NSString>> {
    if is::<NSButton>(this) || is::<NSButtonCell>(this) {
        // SAFETY: buttons and their cells answer `title` with a string.
        Some(unsafe { msg_send![this, title] })
    } else {
        None
    }
}

// The defaults, as macOS gives them.

fn default_role(this: &AnyObject) -> &'static NSString {
    // SAFETY (all the statics): immutable constant strings, exported below
    // and read through their bindings.
    unsafe {
        if let Some(cell) = this.downcast_ref::<NSCell>() {
            if let Some(button) = this.downcast_ref::<NSButtonCell>() {
                return match button::imp(button).look() {
                    Look::Check => ak::NSAccessibilityCheckBoxRole,
                    Look::Radio => ak::NSAccessibilityRadioButtonRole,
                    _ => ak::NSAccessibilityButtonRole,
                };
            }
            if is::<NSSecureTextFieldCell>(this) || is::<NSSearchFieldCell>(this) {
                return ak::NSAccessibilityTextFieldRole;
            }
            if let Some(field) = this.downcast_ref::<NSTextFieldCell>() {
                return if field.isEditable() {
                    ak::NSAccessibilityTextFieldRole
                } else {
                    ak::NSAccessibilityStaticTextRole
                };
            }
            if is::<NSSegmentedCell>(this) {
                return ak::NSAccessibilityRadioGroupRole;
            }
            if is::<NSStepperCell>(this) {
                return ak::NSAccessibilityIncrementorRole;
            }
            if is::<NSSliderCell>(this) {
                return ak::NSAccessibilitySliderRole;
            }
            if cell.r#type() == NSCellType::TextCellType {
                return ak::NSAccessibilityStaticTextRole;
            }
            return ak::NSAccessibilityUnknownRole;
        }
        if is::<NSBox>(this) {
            ak::NSAccessibilityGroupRole
        } else if let Some(indicator) = this.downcast_ref::<NSProgressIndicator>() {
            if indicator.isIndeterminate() {
                ak::NSAccessibilityBusyIndicatorRole
            } else {
                ak::NSAccessibilityProgressIndicatorRole
            }
        } else if is::<NSSwitch>(this) {
            ak::NSAccessibilityButtonRole
        } else {
            ak::NSAccessibilityUnknownRole
        }
    }
}

fn default_subrole(this: &AnyObject) -> Option<&'static NSString> {
    // SAFETY: immutable constant strings, exported below.
    unsafe {
        if is::<NSSwitch>(this) {
            Some(ak::NSAccessibilitySwitchSubrole)
        } else if is::<NSSecureTextFieldCell>(this) {
            Some(ak::NSAccessibilitySecureTextFieldSubrole)
        } else if is::<NSSearchFieldCell>(this) {
            Some(ak::NSAccessibilitySearchFieldSubrole)
        } else {
            None
        }
    }
}

fn default_element(this: &AnyObject) -> bool {
    if is::<NSCell>(this) || is::<NSProgressIndicator>(this) || is::<NSSwitch>(this) {
        true
    } else if let Some(b) = this.downcast_ref::<NSBox>() {
        b.boxType() != NSBoxType::Separator
    } else {
        false
    }
}

fn default_enabled(this: &AnyObject) -> bool {
    if let Some(control) = this.downcast_ref::<NSControl>() {
        control.isEnabled()
    } else if let Some(cell) = this.downcast_ref::<NSCell>() {
        cell.isEnabled()
    } else {
        false
    }
}

fn default_value(this: &AnyObject) -> Option<Retained<AnyObject>> {
    // SAFETY (both): every object is an AnyObject.
    let number = |n: Retained<NSNumber>| Some(unsafe { Retained::cast_unchecked::<AnyObject>(n) });
    let text = |s: Retained<NSString>| Some(unsafe { Retained::cast_unchecked::<AnyObject>(s) });
    if let Some(field) = this.downcast_ref::<NSTextField>() {
        return text(field.stringValue());
    }
    if is::<NSSwitch>(this) {
        // SAFETY: switches answer `state` with an NSInteger.
        let state: isize = unsafe { msg_send![this, state] };
        return number(NSNumber::new_isize(state));
    }
    if let Some(indicator) = this.downcast_ref::<NSProgressIndicator>() {
        if indicator.isIndeterminate() {
            return None;
        }
        // The fraction done.
        let (min, max) = (indicator.minValue(), indicator.maxValue());
        let fraction = if max > min { (indicator.doubleValue() - min) / (max - min) } else { 0.0 };
        return number(NSNumber::new_f64(fraction));
    }
    let cell = this.downcast_ref::<NSCell>()?;
    if let Some(button) = this.downcast_ref::<NSButtonCell>() {
        return match button::imp(button).look() {
            Look::Check | Look::Radio => number(NSNumber::new_isize(cell.state())),
            _ => None,
        };
    }
    if is::<NSStepperCell>(this) || is::<NSSliderCell>(this) {
        return number(NSNumber::new_f64(cell.doubleValue()));
    }
    if is::<NSSegmentedCell>(this) {
        return None;
    }
    if is::<NSTextFieldCell>(this) || cell.r#type() == NSCellType::TextCellType {
        return text(cell.stringValue());
    }
    None
}

// The methods.

macro_rules! object_property {
    ($get:ident, $set:ident, $field:ident: $t:ty, $default:expr) => {
        extern "C-unwind" fn $get(this: &AnyObject, _: Sel) -> *mut $t {
            let value = match read(this, |p| p.$field.clone()) {
                Some(set) => set,
                None => ($default)(this),
            };
            value.map_or(std::ptr::null_mut(), Retained::autorelease_return)
        }

        extern "C-unwind" fn $set(this: &AnyObject, _: Sel, value: Option<&$t>) {
            write(this, |p| &mut p.$field, Some(value.map(|v| v.retain())));
        }
    };
}

macro_rules! bool_property {
    ($get:ident, $set:ident, $field:ident, $default:expr) => {
        extern "C-unwind" fn $get(this: &AnyObject, _: Sel) -> Bool {
            Bool::new(read(this, |p| p.$field).unwrap_or_else(|| ($default)(this)))
        }

        extern "C-unwind" fn $set(this: &AnyObject, _: Sel, value: Bool) {
            write(this, |p| &mut p.$field, Some(value.as_bool()));
        }
    };
}

fn none<T>(_: &AnyObject) -> Option<Retained<T>> {
    None
}

object_property!(role, set_role, role: NSString, |this| string(default_role(this)));
object_property!(subrole, set_subrole, subrole: NSString, |this| default_subrole(this).and_then(string));
object_property!(role_description, set_role_description, role_description: NSString, none);
object_property!(label, set_label, label: NSString, default_label);
object_property!(title, set_title, title: NSString, title_of);
object_property!(help, set_help, help: NSString, none);

/// `setAccessibilityHelp:`: a control's help is its cell's too.
extern "C-unwind" fn set_help_through(this: &AnyObject, cmd: Sel, value: Option<&NSString>) {
    set_help(this, cmd, value);
    if let Some(control) = this.downcast_ref::<NSControl>()
        && let Some(cell) = control.cell()
    {
        set_help(&cell, cmd, value);
    }
}
object_property!(identifier, set_identifier, identifier: NSString, none);
object_property!(placeholder, set_placeholder, placeholder: NSString, none);
object_property!(value, set_value, value: AnyObject, default_value);
object_property!(value_description, set_value_description, value_description: NSString, none);
bool_property!(is_element, set_element, element, default_element);
bool_property!(is_hidden, set_hidden, hidden, |_| false);
bool_property!(is_enabled, set_enabled, enabled, default_enabled);

/// Add `f` to `class` under `sel`, with the type encoding its signature
/// gives.
fn add<F: MethodImplementation>(class: &AnyClass, sel: Sel, f: F) {
    let mut types = F::Return::ENCODING_RETURN.to_string();
    types.push_str("@:");
    for arg in F::Arguments::ENCODINGS {
        types.push_str(&arg.to_string());
    }
    let types = CString::new(types).expect("encodings have no NUL");
    // SAFETY: `MethodImplementation` is implemented only for function
    // pointers, which all have `Imp`'s size and representation.
    let imp: Imp = unsafe { std::mem::transmute_copy(&f) };
    // SAFETY: the function takes the receiver and selector, then the
    // arguments the encoding gives, and returns what it says.
    unsafe { objc2::ffi::class_addMethod((class as *const AnyClass).cast_mut(), sel, imp, types.as_ptr()) };
}

// With one lifetime, not any: `MethodImplementation` is implemented for
// function pointers over a receiver type, not a higher-ranked one.
type Getter<T> = extern "C-unwind" fn(&'static AnyObject, Sel) -> *mut T;
type Setter<T> = extern "C-unwind" fn(&'static AnyObject, Sel, Option<&'static T>);
type BoolGetter = extern "C-unwind" fn(&'static AnyObject, Sel) -> Bool;
type BoolSetter = extern "C-unwind" fn(&'static AnyObject, Sel, Bool);

/// Give `class` (NSView or NSCell) the accessibility properties.
pub(crate) fn install(class: &AnyClass) {
    let strings: [(Sel, Getter<NSString>, Sel, Setter<NSString>); 9] = [
        (sel!(accessibilityRole), role, sel!(setAccessibilityRole:), set_role),
        (sel!(accessibilitySubrole), subrole, sel!(setAccessibilitySubrole:), set_subrole),
        (
            sel!(accessibilityRoleDescription),
            role_description,
            sel!(setAccessibilityRoleDescription:),
            set_role_description,
        ),
        (sel!(accessibilityLabel), label, sel!(setAccessibilityLabel:), set_label),
        (sel!(accessibilityTitle), title, sel!(setAccessibilityTitle:), set_title),
        (sel!(accessibilityHelp), help, sel!(setAccessibilityHelp:), set_help_through),
        (sel!(accessibilityIdentifier), identifier, sel!(setAccessibilityIdentifier:), set_identifier),
        (sel!(accessibilityPlaceholderValue), placeholder, sel!(setAccessibilityPlaceholderValue:), set_placeholder),
        (
            sel!(accessibilityValueDescription),
            value_description,
            sel!(setAccessibilityValueDescription:),
            set_value_description,
        ),
    ];
    for (get_sel, get, set_sel, set) in strings {
        add(class, get_sel, get);
        add(class, set_sel, set);
    }
    add(class, sel!(accessibilityValue), value as Getter<AnyObject>);
    add(class, sel!(setAccessibilityValue:), set_value as Setter<AnyObject>);
    let bools: [(Sel, BoolGetter, Sel, BoolSetter); 3] = [
        (sel!(isAccessibilityElement), is_element, sel!(setAccessibilityElement:), set_element),
        (sel!(isAccessibilityHidden), is_hidden, sel!(setAccessibilityHidden:), set_hidden),
        (sel!(isAccessibilityEnabled), is_enabled, sel!(setAccessibilityEnabled:), set_enabled),
    ];
    for (get_sel, get, set_sel, set) in bools {
        add(class, get_sel, get);
        add(class, set_sel, set);
    }
}

// The roles and subroles, with macOS's values.

macro_rules! constants {
    ($($name:ident = $value:literal,)*) => {
        $(sidestep_foundation::constant_string!($name = $value);)*
    };
}

constants! {
    NSAccessibilityApplicationRole = "AXApplication",
    NSAccessibilityBusyIndicatorRole = "AXBusyIndicator",
    NSAccessibilityButtonRole = "AXButton",
    NSAccessibilityCellRole = "AXCell",
    NSAccessibilityCheckBoxRole = "AXCheckBox",
    NSAccessibilityColumnRole = "AXColumn",
    NSAccessibilityComboBoxRole = "AXComboBox",
    NSAccessibilityDisclosureTriangleRole = "AXDisclosureTriangle",
    NSAccessibilityGroupRole = "AXGroup",
    NSAccessibilityHeadingRole = "AXHeading",
    NSAccessibilityImageRole = "AXImage",
    NSAccessibilityIncrementorRole = "AXIncrementor",
    NSAccessibilityLayoutAreaRole = "AXLayoutArea",
    NSAccessibilityLevelIndicatorRole = "AXLevelIndicator",
    NSAccessibilityLinkRole = "AXLink",
    NSAccessibilityListRole = "AXList",
    NSAccessibilityMenuBarItemRole = "AXMenuBarItem",
    NSAccessibilityMenuBarRole = "AXMenuBar",
    NSAccessibilityMenuButtonRole = "AXMenuButton",
    NSAccessibilityMenuItemRole = "AXMenuItem",
    NSAccessibilityMenuRole = "AXMenu",
    NSAccessibilityOutlineRole = "AXOutline",
    NSAccessibilityPopoverRole = "AXPopover",
    NSAccessibilityPopUpButtonRole = "AXPopUpButton",
    NSAccessibilityProgressIndicatorRole = "AXProgressIndicator",
    NSAccessibilityRadioButtonRole = "AXRadioButton",
    NSAccessibilityRadioGroupRole = "AXRadioGroup",
    NSAccessibilityRowRole = "AXRow",
    NSAccessibilityScrollAreaRole = "AXScrollArea",
    NSAccessibilityScrollBarRole = "AXScrollBar",
    NSAccessibilitySheetRole = "AXSheet",
    NSAccessibilitySliderRole = "AXSlider",
    NSAccessibilitySplitGroupRole = "AXSplitGroup",
    NSAccessibilitySplitterRole = "AXSplitter",
    NSAccessibilityStaticTextRole = "AXStaticText",
    NSAccessibilityTabGroupRole = "AXTabGroup",
    NSAccessibilityTableRole = "AXTable",
    NSAccessibilityTextAreaRole = "AXTextArea",
    NSAccessibilityTextFieldRole = "AXTextField",
    NSAccessibilityToolbarRole = "AXToolbar",
    NSAccessibilityUnknownRole = "AXUnknown",
    NSAccessibilityValueIndicatorRole = "AXValueIndicator",
    NSAccessibilityWindowRole = "AXWindow",
    NSAccessibilitySearchFieldSubrole = "AXSearchField",
    NSAccessibilitySecureTextFieldSubrole = "AXSecureTextField",
    NSAccessibilitySwitchSubrole = "AXSwitch",
    NSAccessibilityToggleSubrole = "AXToggle",
}

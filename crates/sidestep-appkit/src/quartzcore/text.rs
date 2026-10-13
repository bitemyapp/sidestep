//! Text layers draw through the same shaped-text pipeline as attributed strings.
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSFont, NSLineBreakMode, NSMutableParagraphStyle, NSStringDrawingOptions, NSTextAlignment,
};
use objc2_core_foundation::CFType;
use objc2_core_graphics::{CGColor, CGContext};
use objc2_foundation::{NSAttributedString, NSCopying, NSDictionary, NSString};
use objc2_quartz_core::{CALayer, CATextLayer};
use std::cell::RefCell;

sidestep_runtime::static_class!(pub(crate) TEXT, TEXT_META = "CATextLayer", || { let _ = Text::class(); });
sidestep_foundation::constant_string!(kCAAlignmentNatural = "natural");
sidestep_foundation::constant_string!(kCAAlignmentLeft = "left");
sidestep_foundation::constant_string!(kCAAlignmentRight = "right");
sidestep_foundation::constant_string!(kCAAlignmentCenter = "center");
sidestep_foundation::constant_string!(kCAAlignmentJustified = "justified");
sidestep_foundation::constant_string!(kCATruncationNone = "none");
sidestep_foundation::constant_string!(kCATruncationStart = "start");
sidestep_foundation::constant_string!(kCATruncationEnd = "end");
sidestep_foundation::constant_string!(kCATruncationMiddle = "middle");
#[derive(Clone)]
struct State {
    string: Option<Retained<AnyObject>>,
    font: Option<Retained<CFType>>,
    size: f64,
    color: Option<Retained<CGColor>>,
    align: Retained<NSString>,
    truncation: Retained<NSString>,
    wrapped: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            string: None,
            font: None,
            size: 36.0,
            color: Some(super::objects::color([1.0; 4])),
            align: NSString::from_str("natural"),
            truncation: NSString::from_str("none"),
            wrapped: false,
        }
    }
}
define_class!(
    #[unsafe(super(CALayer, NSObject))]
    #[name = "CATextLayer"]
    #[ivars = RefCell<State>]
    struct Text;
    impl Text {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> { let this = this.set_ivars(RefCell::new(State::default())); let this: Retained<Self> = unsafe { msg_send![super(this), init] }; this.setNeedsDisplayOnBoundsChange(true); this }
        #[unsafe(method_id(initWithLayer:))]
        fn copy(this: Allocated<Self>, layer: &AnyObject) -> Retained<Self> {
            let state = layer.downcast_ref::<CATextLayer>().map(|l| unsafe { &*(l as *const CATextLayer).cast::<Text>() }.ivars().borrow().clone()).unwrap_or_default();
            let this = this.set_ivars(RefCell::new(state)); unsafe { msg_send![super(this), initWithLayer: layer] }
        }
        #[unsafe(method_id(string))]
        fn string(&self) -> Option<Retained<AnyObject>> { self.ivars().borrow().string.clone() }
        #[unsafe(method(setString:))]
        fn set_string(&self, value: Option<&AnyObject>) { let copied = value.map(|v| unsafe { msg_send![v, copy] }); self.ivars().borrow_mut().string = copied; self.setNeedsDisplay(); }
        #[unsafe(method(font))]
        fn font(&self) -> *mut CFType { super::layer::autoreleased(self.ivars().borrow().font.clone()) }
        #[unsafe(method(setFont:))]
        fn set_font(&self, value: Option<&CFType>) { self.ivars().borrow_mut().font = value.map(Message::retain); self.setNeedsDisplay(); }
        #[unsafe(method(fontSize))]
        fn size(&self) -> f64 { self.ivars().borrow().size }
        #[unsafe(method(setFontSize:))]
        fn set_size(&self, value: f64) { self.ivars().borrow_mut().size = value; self.setNeedsDisplay(); }
        #[unsafe(method(foregroundColor))]
        fn color(&self) -> *mut CGColor { super::layer::autoreleased(self.ivars().borrow().color.clone()) }
        #[unsafe(method(setForegroundColor:))]
        fn set_color(&self, value: Option<&CGColor>) { self.ivars().borrow_mut().color = value.map(Message::retain); self.setNeedsDisplay(); }
        #[unsafe(method_id(alignmentMode))]
        fn align(&self) -> Retained<NSString> { self.ivars().borrow().align.clone() }
        #[unsafe(method(setAlignmentMode:))]
        fn set_align(&self, value: &NSString) { self.ivars().borrow_mut().align = value.copy(); self.setNeedsDisplay(); }
        #[unsafe(method_id(truncationMode))]
        fn truncation(&self) -> Retained<NSString> { self.ivars().borrow().truncation.clone() }
        #[unsafe(method(setTruncationMode:))]
        fn set_truncation(&self, value: &NSString) { self.ivars().borrow_mut().truncation = value.copy(); self.setNeedsDisplay(); }
        #[unsafe(method(isWrapped))]
        fn wrapped(&self) -> bool { self.ivars().borrow().wrapped }
        #[unsafe(method(setWrapped:))]
        fn set_wrapped(&self, value: bool) { self.ivars().borrow_mut().wrapped = value; self.setNeedsDisplay(); }
        #[unsafe(method(drawInContext:))]
        fn draw(&self, context: &CGContext) { draw(self, context); }
    }
);
fn draw(layer: &Text, context: &CGContext) {
    let s = layer.ivars().borrow().clone();
    let Some(string) = s.string else { return };
    let bounds = layer.bounds();
    let mut options = NSStringDrawingOptions::empty();
    if s.wrapped {
        options |= NSStringDrawingOptions::UsesLineFragmentOrigin;
    }
    if s.truncation.to_string() != "none" {
        options |= NSStringDrawingOptions::TruncatesLastVisibleLine;
    }
    crate::coregraphics::context::drawing_into(context, || {
        if let Some(attributed) = string.downcast_ref::<NSAttributedString>() {
            let _: () = unsafe { msg_send![attributed, drawWithRect: bounds, options: options] };
            return;
        }
        let Some(string) = string.downcast_ref::<NSString>() else { return };
        let font_object = s.font.as_ref().map(|f| unsafe { &*(Retained::as_ptr(f).cast::<AnyObject>()) });
        let name = font_object.and_then(|f| {
            f.downcast_ref::<NSFont>()
                .map(|font| font.fontName())
                .or_else(|| f.downcast_ref::<NSString>().map(Message::retain))
        });
        let font = name
            .and_then(|name| NSFont::fontWithName_size(&name, s.size))
            .unwrap_or_else(|| NSFont::systemFontOfSize(s.size));
        let color = s.color.as_ref().and_then(|c| NSColor::colorWithCGColor(c)).unwrap_or_else(NSColor::clearColor);
        let paragraph = NSMutableParagraphStyle::new();
        paragraph.setAlignment(match s.align.to_string().as_str() {
            "center" => NSTextAlignment::Center,
            "right" => NSTextAlignment::Right,
            "left" => NSTextAlignment::Left,
            "justified" => NSTextAlignment::Justified,
            _ => NSTextAlignment::Natural,
        });
        paragraph.setLineBreakMode(match s.truncation.to_string().as_str() {
            "start" => NSLineBreakMode::ByTruncatingHead,
            "middle" => NSLineBreakMode::ByTruncatingMiddle,
            "end" => NSLineBreakMode::ByTruncatingTail,
            _ if s.wrapped => NSLineBreakMode::ByWordWrapping,
            _ => NSLineBreakMode::ByClipping,
        });
        let attrs: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
            &[
                unsafe { objc2_app_kit::NSFontAttributeName },
                unsafe { objc2_app_kit::NSForegroundColorAttributeName },
                unsafe { objc2_app_kit::NSParagraphStyleAttributeName },
            ],
            &[font.into(), color.into(), paragraph.into()],
        );
        let _: () = unsafe { msg_send![string, drawWithRect: bounds, options: options, attributes: &*attrs] };
    });
}

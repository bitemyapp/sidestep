//! Print settings and paginated PDF output. Pages are rendered by Sidestep at
//! 144 dpi; saved PDFs retain page geometry but currently contain raster pages.
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSPrintInfo, NSTextView, NSView};
use objc2_foundation::{
    NSDictionary, NSMutableDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString, NSURL, NSValue, NSZone,
};
use std::cell::{Cell, RefCell};
use std::sync::Mutex;

sidestep_runtime::static_class!(pub(crate) INFO, INFO_META = "NSPrintInfo", || { let _ = Info::class(); });
sidestep_runtime::static_class!(pub(crate) OPERATION, OPERATION_META = "NSPrintOperation", || { let _ = Operation::class(); });
sidestep_foundation::constant_string!(NSPrintSaveJob = "NSPrintSaveJob");
sidestep_foundation::constant_string!(NSPrintSpoolJob = "NSPrintSpoolJob");
sidestep_foundation::constant_string!(NSPrintCancelJob = "NSPrintCancelJob");
sidestep_foundation::constant_string!(NSPrintJobDisposition = "NSPrintJobDisposition");
sidestep_foundation::constant_string!(NSPrintJobSavingURL = "NSPrintJobSavingURL");
sidestep_foundation::constant_string!(NSPrintPaperSize = "NSPrintPaperSize");
sidestep_foundation::constant_string!(NSPrintOrientation = "NSPrintOrientation");
sidestep_foundation::constant_string!(NSPrintTopMargin = "NSPrintTopMargin");
sidestep_foundation::constant_string!(NSPrintBottomMargin = "NSPrintBottomMargin");
sidestep_foundation::constant_string!(NSPrintLeftMargin = "NSPrintLeftMargin");
sidestep_foundation::constant_string!(NSPrintRightMargin = "NSPrintRightMargin");
sidestep_foundation::constant_string!(NSPrintHorizontalPagination = "NSPrintHorizontalPagination");
sidestep_foundation::constant_string!(NSPrintVerticalPagination = "NSPrintVerticalPagination");
sidestep_foundation::constant_string!(NSPrintPreviewJob = "NSPrintPreviewJob");
sidestep_foundation::constant_string!(
    NSPrintJobSavingFileNameExtensionHidden = "NSPrintJobSavingFileNameExtensionHidden"
);
sidestep_foundation::constant_string!(NSPrintPaperName = "NSPrintPaperName");
sidestep_foundation::constant_string!(NSPrintScalingFactor = "NSPrintScalingFactor");
sidestep_foundation::constant_string!(NSPrintHorizontallyCentered = "NSPrintHorizontallyCentered");
sidestep_foundation::constant_string!(NSPrintVerticallyCentered = "NSPrintVerticallyCentered");
sidestep_foundation::constant_string!(NSPrintPrinter = "NSPrintPrinter");
sidestep_foundation::constant_string!(NSPrintCopies = "NSPrintCopies");
sidestep_foundation::constant_string!(NSPrintAllPages = "NSPrintAllPages");
sidestep_foundation::constant_string!(NSPrintFirstPage = "NSPrintFirstPage");
sidestep_foundation::constant_string!(NSPrintLastPage = "NSPrintLastPage");
sidestep_foundation::constant_string!(NSPrintMustCollate = "NSPrintMustCollate");
sidestep_foundation::constant_string!(NSPrintReversePageOrder = "NSPrintReversePageOrder");
sidestep_foundation::constant_string!(NSPrintSavePath = "NSPrintSavePath");
sidestep_foundation::constant_string!(NSPrintPagesAcross = "NSPrintPagesAcross");
sidestep_foundation::constant_string!(NSPrintPagesDown = "NSPrintPagesDown");
sidestep_foundation::constant_string!(NSPrintTime = "NSPrintTime");
sidestep_foundation::constant_string!(NSPrintDetailedErrorReporting = "NSPrintDetailedErrorReporting");
sidestep_foundation::constant_string!(NSPrintFaxNumber = "NSPrintFaxNumber");
sidestep_foundation::constant_string!(NSPrintPrinterName = "NSPrintPrinterName");
sidestep_foundation::constant_string!(NSPrintSelectionOnly = "NSPrintSelectionOnly");
sidestep_foundation::constant_string!(NSPrintHeaderAndFooter = "NSPrintHeaderAndFooter");
sidestep_foundation::constant_string!(NSPrintPagesPerSheet = "NSPrintPagesPerSheet");
sidestep_foundation::constant_string!(NSPrintJobFeatures = "NSPrintJobFeatures");
sidestep_foundation::constant_string!(NSPrintManualFeed = "NSPrintManualFeed");
sidestep_foundation::constant_string!(NSPrintPaperFeed = "NSPrintPaperFeed");
sidestep_foundation::constant_string!(NSPrintFormName = "NSPrintFormName");
sidestep_foundation::constant_string!(NSPrintOperationExistsException = "NSPrintOperationExistsException");
type Dict = NSMutableDictionary<NSString, AnyObject>;
fn set(dict: &Dict, key: &str, value: &AnyObject) {
    let _: () = unsafe { msg_send![dict, setObject: value, forKey: &*NSString::from_str(key)] };
}
fn get(dict: &Dict, key: &str) -> Option<Retained<AnyObject>> {
    dict.objectForKey(&NSString::from_str(key))
}
fn number(dict: &Dict, key: &str, default: f64) -> f64 {
    get(dict, key).and_then(|o| o.downcast::<NSNumber>().ok()).map_or(default, |n| n.doubleValue())
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSPrintInfo"]
    #[ivars = Mutex<Retained<Dict>>]
    struct Info;
    impl Info {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> { let this = this.set_ivars(Mutex::new(Dict::new())); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(initWithDictionary:))]
        fn with_dict(this: Allocated<Self>, dict: &NSDictionary<NSString, AnyObject>) -> Retained<Self> { let copy: Retained<Dict> = unsafe { msg_send![dict, mutableCopy] }; let this = this.set_ivars(Mutex::new(copy)); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(sharedPrintInfo))]
        fn shared() -> Retained<Self> { shared_info() }
        #[unsafe(method_id(copyWithZone:))]
        fn copy(&self, _zone: *mut NSZone) -> Retained<Self> { let dict = self.ivars().lock().unwrap().clone(); unsafe { msg_send![Self::alloc(), initWithDictionary: &*dict] } }
        #[unsafe(method_id(dictionary))]
        fn dict(&self) -> Retained<Dict> { self.ivars().lock().unwrap().clone() }
        #[unsafe(method_id(jobDisposition))]
        fn job(&self) -> Retained<NSString> { get(&self.ivars().lock().unwrap(), "NSPrintJobDisposition").and_then(|o| o.downcast::<NSString>().ok()).unwrap_or_else(|| NSString::from_str("NSPrintSpoolJob")) }
        #[unsafe(method(setJobDisposition:))]
        fn set_job(&self, job: &NSString) { set(&self.ivars().lock().unwrap(), "NSPrintJobDisposition", job); }
        #[unsafe(method(paperSize))]
        fn paper(&self) -> NSSize { paper(self) }
        #[unsafe(method(setPaperSize:))]
        fn set_paper(&self, size: NSSize) { set(&self.ivars().lock().unwrap(), "NSPrintPaperSize", &*unsafe { NSValue::valueWithSize(size) }); }
        #[unsafe(method(topMargin))]
        fn topmargin(&self) -> f64 { number(&self.ivars().lock().unwrap(), "NSPrintTopMargin", 36.0) }
        #[unsafe(method(setTopMargin:))]
        fn set_topmargin(&self, value: f64) { set(&self.ivars().lock().unwrap(), "NSPrintTopMargin", &NSNumber::numberWithDouble(value as f64)); }
        #[unsafe(method(bottomMargin))]
        fn bottommargin(&self) -> f64 { number(&self.ivars().lock().unwrap(), "NSPrintBottomMargin", 36.0) }
        #[unsafe(method(setBottomMargin:))]
        fn set_bottommargin(&self, value: f64) { set(&self.ivars().lock().unwrap(), "NSPrintBottomMargin", &NSNumber::numberWithDouble(value as f64)); }
        #[unsafe(method(leftMargin))]
        fn leftmargin(&self) -> f64 { number(&self.ivars().lock().unwrap(), "NSPrintLeftMargin", 36.0) }
        #[unsafe(method(setLeftMargin:))]
        fn set_leftmargin(&self, value: f64) { set(&self.ivars().lock().unwrap(), "NSPrintLeftMargin", &NSNumber::numberWithDouble(value as f64)); }
        #[unsafe(method(rightMargin))]
        fn rightmargin(&self) -> f64 { number(&self.ivars().lock().unwrap(), "NSPrintRightMargin", 36.0) }
        #[unsafe(method(setRightMargin:))]
        fn set_rightmargin(&self, value: f64) { set(&self.ivars().lock().unwrap(), "NSPrintRightMargin", &NSNumber::numberWithDouble(value as f64)); }
        #[unsafe(method(horizontalPagination))]
        fn horizontalpagination(&self) -> usize { number(&self.ivars().lock().unwrap(), "NSPrintHorizontalPagination", 0.0) as usize }
        #[unsafe(method(setHorizontalPagination:))]
        fn set_horizontalpagination(&self, value: usize) { set(&self.ivars().lock().unwrap(), "NSPrintHorizontalPagination", &NSNumber::numberWithDouble(value as f64)); }
        #[unsafe(method(verticalPagination))]
        fn verticalpagination(&self) -> usize { number(&self.ivars().lock().unwrap(), "NSPrintVerticalPagination", 0.0) as usize }
        #[unsafe(method(setVerticalPagination:))]
        fn set_verticalpagination(&self, value: usize) { set(&self.ivars().lock().unwrap(), "NSPrintVerticalPagination", &NSNumber::numberWithDouble(value as f64)); }
        #[unsafe(method(orientation))]
        fn orientation(&self) -> usize { number(&self.ivars().lock().unwrap(), "NSPrintOrientation", 0.0) as usize }
        #[unsafe(method(setOrientation:))]
        fn set_orientation(&self, value: usize) { set(&self.ivars().lock().unwrap(), "NSPrintOrientation", &NSNumber::numberWithDouble(value as f64)); }
    }
    unsafe impl NSObjectProtocol for Info {}
);
fn shared_info() -> Retained<Info> {
    static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = SHARED.get_or_init(|| {
        let info: Retained<Info> = unsafe { msg_send![Info::alloc(), init] };
        Retained::into_raw(info) as usize
    });
    unsafe { Retained::retain(*p as *mut Info).unwrap() }
}
fn paper(info: &Info) -> NSSize {
    get(&info.ivars().lock().unwrap(), "NSPrintPaperSize")
        .and_then(|o| o.downcast::<NSValue>().ok())
        .map_or(NSSize::new(612.0, 792.0), |v| unsafe { v.sizeValue() })
}
struct OperationState {
    view: Retained<NSView>,
    info: Retained<NSPrintInfo>,
    title: RefCell<Option<Retained<NSString>>>,
    print_panel: Cell<bool>,
    progress_panel: Cell<bool>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSPrintOperation"]
    #[ivars = OperationState]
    struct Operation;
    impl Operation {
        #[unsafe(method_id(printOperationWithView:printInfo:))]
        fn with_view(view: &NSView, info: &NSPrintInfo) -> Retained<Self> { operation(view, info) }
        #[unsafe(method_id(printOperationWithView:))]
        fn default_info(view: &NSView) -> Retained<Self> { operation(view, &NSPrintInfo::sharedPrintInfo()) }
        #[unsafe(method_id(view))]
        fn view(&self) -> Retained<NSView> { self.ivars().view.clone() }
        #[unsafe(method_id(printInfo))]
        fn info(&self) -> Retained<NSPrintInfo> { self.ivars().info.clone() }
        #[unsafe(method_id(jobTitle))]
        fn title(&self) -> Option<Retained<NSString>> { self.ivars().title.borrow().clone() }
        #[unsafe(method(setJobTitle:))]
        fn set_title(&self, title: Option<&NSString>) { self.ivars().title.replace(title.map(Message::retain)); }
        #[unsafe(method(showsPrintPanel))]
        fn print_panel(&self) -> bool { self.ivars().print_panel.get() }
        #[unsafe(method(setShowsPrintPanel:))]
        fn set_print_panel(&self, show: bool) { self.ivars().print_panel.set(show); }
        #[unsafe(method(showsProgressPanel))]
        fn progress_panel(&self) -> bool { self.ivars().progress_panel.get() }
        #[unsafe(method(setShowsProgressPanel:))]
        fn set_progress_panel(&self, show: bool) { self.ivars().progress_panel.set(show); }
        #[unsafe(method(runOperation))]
        fn run(&self) -> bool { run(self) }
    }
    unsafe impl NSObjectProtocol for Operation {}
);
fn operation(view: &NSView, info: &NSPrintInfo) -> Retained<Operation> {
    let info: Retained<NSPrintInfo> = unsafe { msg_send![info, copy] };
    let this = Operation::alloc(view.mtm()).set_ivars(OperationState {
        view: view.retain(),
        info,
        title: RefCell::new(None),
        print_panel: Cell::new(true),
        progress_panel: Cell::new(true),
    });
    unsafe { msg_send![super(this), init] }
}
fn run(operation: &Operation) -> bool {
    let info = &operation.ivars().info;
    let job = info.jobDisposition().to_string();
    if job == "NSPrintCancelJob" || (job != "NSPrintSaveJob" && operation.ivars().print_panel.get()) {
        return false;
    }
    let Some(pdf) = pdf(&operation.ivars().view, info) else { return false };
    if job == "NSPrintSaveJob" {
        let dictionary: Retained<Dict> = unsafe { msg_send![&**info, dictionary] };
        let Some(url) = get(&dictionary, "NSPrintJobSavingURL").and_then(|o| o.downcast::<NSURL>().ok()) else {
            return false;
        };
        return objc2_foundation::NSData::with_bytes(&pdf)
            .writeToURL_options_error(&url, objc2_foundation::NSDataWritingOptions::Atomic)
            .is_ok();
    }
    if job != "NSPrintSpoolJob" {
        return false;
    }
    let path =
        std::env::temp_dir().join(format!("sidestep-print-{}.pdf", objc2_foundation::NSUUID::UUID().UUIDString()));
    let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).open(&path) else { return false };
    use std::io::Write;
    let written = file.write_all(&pdf).is_ok();
    drop(file);
    let result = written
        && std::process::Command::new("lp")
            .arg("-t")
            .arg(operation.ivars().title.borrow().as_ref().map_or_else(|| "Document".into(), ToString::to_string))
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(path);
    result
}
fn pdf(view: &NSView, info: &NSPrintInfo) -> Option<Vec<u8>> {
    let paper = info.paperSize();
    let (width, height) =
        (paper.width - info.leftMargin() - info.rightMargin(), paper.height - info.topMargin() - info.bottomMargin());
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let bounds = view.bounds();
    let mut extent = bounds.size.height;
    if let Some(text) = view.downcast_ref::<NSTextView>()
        && let (Some(manager), Some(container)) = (unsafe { text.layoutManager() }, unsafe { text.textContainer() })
    {
        manager.ensureLayoutForTextContainer(&container);
        extent = extent.max(manager.usedRectForTextContainer(&container).size.height);
    }
    let scale = if bounds.size.width > width { width / bounds.size.width } else { 1.0 };
    let slice = height / scale;
    let pages = (extent / slice).ceil().max(1.0) as usize;
    if pages > 1000 || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let mut objects: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), Vec::new()];
    let kids = (0..pages).map(|i| format!("{} 0 R", 3 + i * 3)).collect::<Vec<_>>().join(" ");
    objects[1] = format!("<< /Type /Pages /Count {pages} /Kids [{kids}] >>").into_bytes();
    // Temporarily extend a text view so its normal display code can draw every page.
    struct Restore<'a>(&'a NSView, NSRect);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            self.0.setFrame(self.1);
        }
    }
    let _restore = Restore(view, view.frame());
    if extent > bounds.size.height {
        view.setFrameSize(NSSize::new(view.frame().size.width, extent));
    }
    for index in 0..pages {
        let draw_height = slice.min((extent - index as f64 * slice).max(0.0));
        let source = NSRect::new(
            NSPoint::new(
                bounds.origin.x,
                bounds.origin.y
                    + if view.isFlipped() {
                        index as f64 * slice
                    } else {
                        (extent - (index as f64 + 1.0) * slice).max(0.0)
                    },
            ),
            NSSize::new(bounds.size.width, draw_height),
        );
        let pw = (source.size.width * scale * 2.0).ceil() as usize;
        let ph = (source.size.height * scale * 2.0).ceil() as usize;
        let bitmap = crate::image_rep::new_bitmap(pw.max(1), ph.max(1), source.size)?;
        crate::context::cache_display(crate::views::imp(view), source, &bitmap);
        let image = crate::bitmap::image_data(&bitmap)?;
        let crate::raster::images::Pixels::Rgba(bytes) = &image.pixels else { return None };
        let rgb: Vec<u8> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let white = 255 - p[3];
                [p[0].saturating_add(white), p[1].saturating_add(white), p[2].saturating_add(white)]
            })
            .collect();
        let image_id = objects.len() + 2;
        let content_id = image_id + 1;
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << /Im {image_id} 0 R >> >> /Contents {content_id} 0 R >>", paper.width,paper.height).into_bytes());
        let mut image_object=format!("<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n",image.width,image.height,rgb.len()).into_bytes();
        image_object.extend_from_slice(&rgb);
        image_object.extend_from_slice(b"\nendstream");
        objects.push(image_object);
        let commands = format!(
            "q {} 0 0 {} {} {} cm /Im Do Q",
            source.size.width * scale,
            draw_height * scale,
            info.leftMargin(),
            paper.height - info.topMargin() - draw_height * scale
        );
        objects.push(format!("<< /Length {} >>\nstream\n{commands}\nendstream", commands.len()).into_bytes());
    }
    let mut pdf = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len()).as_bytes(),
    );
    Some(pdf)
}

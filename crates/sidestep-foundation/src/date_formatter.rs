//! `NSFormatter` and `NSDateFormatter`.
//!
//! A date formatter holds a pattern (set, or made from its date and time
//! styles, or from a template), a locale, a time zone and a few switches,
//! behind a lock so it can be shared as on macOS. Formatting and parsing
//! are `date_format`'s. With relative formatting on, dates today,
//! yesterday or tomorrow in the formatter's zone read as those words.

use std::sync::Mutex;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSDate, NSError, NSFormatter, NSInteger, NSLocale, NSString, NSTimeZone, NSUInteger, NSZone};

use crate::date_format::{self, Symbols, Token};
use crate::time_zone::Zone;

sidestep_runtime::static_class!(pub(crate) NSFORMATTER, NSFORMATTER_META = "NSFormatter", || {
    let _ = NSFormatterImpl::class();
    crate::perform::install();
});

sidestep_runtime::static_class!(pub(crate) NSDATEFORMATTER, NSDATEFORMATTER_META = "NSDateFormatter", || {
    let _ = NSDateFormatterImpl::class();
    crate::perform::install();
});

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSFormatter"]
    pub(crate) struct NSFormatterImpl;

    impl NSFormatterImpl {
        #[unsafe(method_id(stringForObjectValue:))]
        fn string_for_object_value(&self, _value: Option<&AnyObject>) -> Option<Retained<NSString>> {
            None
        }

        #[unsafe(method_id(editingStringForObjectValue:))]
        fn editing_string_for_object_value(&self, value: &AnyObject) -> Option<Retained<NSString>> {
            // SAFETY: -stringForObjectValue: takes an object.
            unsafe { msg_send![self, stringForObjectValue: value] }
        }

        #[unsafe(method_id(attributedStringForObjectValue:withDefaultAttributes:))]
        fn attributed_string(&self, _value: &AnyObject, _attributes: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(getObjectValue:forString:errorDescription:))]
        fn get_object_value(&self, _out: *mut *mut AnyObject, _string: &NSString, _error: *mut *mut NSString) -> bool {
            false
        }

        #[unsafe(method(isPartialStringValid:newEditingString:errorDescription:))]
        fn is_partial_string_valid(&self, _partial: &NSString, _new: *mut *mut NSString, _error: *mut *mut NSString) -> bool {
            true
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSFormatterImpl {}
);

/// `NSDateFormatterStyle`.
type Style = NSUInteger;

#[derive(Clone)]
pub(crate) struct State {
    date_style: Style,
    time_style: Style,
    /// A pattern set directly or from a template; styles otherwise.
    format: Option<String>,
    locale: Option<Retained<NSLocale>>,
    time_zone: Option<Retained<NSTimeZone>>,
    lenient: bool,
    relative: bool,
    two_digit_start: i64,
    default_date: Option<f64>,
    symbols: Symbols,
    behavior: NSUInteger,
    generates_calendar_dates: bool,
    formatting_context: NSInteger,
}

impl Default for State {
    fn default() -> Self {
        State {
            date_style: 0,
            time_style: 0,
            format: None,
            locale: None,
            time_zone: None,
            lenient: false,
            relative: false,
            two_digit_start: 1950,
            default_date: None,
            symbols: Symbols::default(),
            behavior: 1040,
            generates_calendar_dates: false,
            formatting_context: 0,
        }
    }
}

impl State {
    fn pattern(&self) -> String {
        self.format
            .clone()
            .unwrap_or_else(|| date_format::style_pattern(self.date_style.min(4), self.time_style.min(4)))
    }

    fn zone(&self) -> Zone {
        match &self.time_zone {
            Some(zone) => crate::time_zone::zone_of(zone).clone(),
            None => crate::time_zone::default_zone(),
        }
    }

    fn format(&self, seconds: f64) -> String {
        let zone = self.zone();
        if self.relative
            && self.format.is_none()
            && self.date_style != 0
            && let Some(word) = relative_day(seconds, &zone)
        {
            if self.time_style == 0 {
                return word.into();
            }
            let time = date_format::style_pattern(0, self.time_style.min(4));
            let time = date_format::format(&date_format::tokenize(&time), seconds, &zone, &self.symbols);
            let joiner = if self.date_style == 1 { ", " } else { " at " };
            return format!("{word}{joiner}{time}");
        }
        date_format::format(&date_format::tokenize(&self.pattern()), seconds, &zone, &self.symbols)
    }

    fn parse(&self, text: &str) -> Option<f64> {
        let tokens: Vec<Token> = date_format::tokenize(&self.pattern());
        date_format::parse(&tokens, text, &self.zone(), &self.symbols, self.lenient, self.two_digit_start)
    }
}

/// "Today", "Yesterday" or "Tomorrow" for a moment, in a zone.
fn relative_day(seconds: f64, zone: &Zone) -> Option<&'static str> {
    let now = crate::date::now();
    let day = |s: f64| date_format::local(s, zone.at(s).offset).days;
    match day(seconds) - day(now) {
        0 => Some("Today"),
        -1 => Some("Yesterday"),
        1 => Some("Tomorrow"),
        _ => None,
    }
}

pub(crate) struct FormatterIvars {
    state: Mutex<State>,
}

define_class!(
    #[unsafe(super(NSFormatter, NSObject))]
    #[name = "NSDateFormatter"]
    #[ivars = FormatterIvars]
    pub(crate) struct NSDateFormatterImpl;

    impl NSDateFormatterImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_state(this, State::default())
        }

        #[unsafe(method_id(initWithDateFormat:allowNaturalLanguage:))]
        fn init_with_date_format(this: Allocated<Self>, format: &NSString, _natural: bool) -> Retained<Self> {
            init_state(this, State { format: Some(format.to_string()), ..State::default() })
        }

        #[unsafe(method_id(localizedStringFromDate:dateStyle:timeStyle:))]
        fn localized_string(date: &NSDate, date_style: Style, time_style: Style) -> Retained<NSString> {
            let state = State { date_style, time_style, ..State::default() };
            NSString::from_str(&state.format(crate::date::time_of(date)))
        }

        #[unsafe(method_id(dateFormatFromTemplate:options:locale:))]
        fn date_format_from_template(template: &NSString, _options: NSUInteger, _locale: Option<&NSLocale>) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&date_format::pattern_from_template(&template.to_string())))
        }

        #[unsafe(method(defaultFormatterBehavior))]
        fn default_formatter_behavior() -> NSUInteger {
            1040
        }

        #[unsafe(method(setDefaultFormatterBehavior:))]
        fn set_default_formatter_behavior(_behavior: NSUInteger) {}

        #[unsafe(method_id(stringFromDate:))]
        fn string_from_date(&self, date: &NSDate) -> Retained<NSString> {
            NSString::from_str(&self.state().format(crate::date::time_of(date)))
        }

        #[unsafe(method_id(dateFromString:))]
        fn date_from_string(&self, text: &NSString) -> Option<Retained<NSDate>> {
            self.state().parse(&text.to_string()).map(NSDate::dateWithTimeIntervalSinceReferenceDate)
        }

        #[unsafe(method_id(stringForObjectValue:))]
        fn string_for_object_value(&self, value: Option<&AnyObject>) -> Option<Retained<NSString>> {
            let date = value.and_then(|v| v.downcast_ref::<NSDate>());
            date.map(|d| NSString::from_str(&self.state().format(crate::date::time_of(d))))
        }

        #[unsafe(method(getObjectValue:forString:errorDescription:))]
        fn get_object_value(&self, out: *mut *mut AnyObject, text: &NSString, error: *mut *mut NSString) -> bool {
            // SAFETY: the caller passes null or room for an object and a
            // string.
            unsafe { self.parse_into(out, &text.to_string(), error) }
        }

        #[unsafe(method(getObjectValue:forString:range:error:))]
        fn get_object_value_range(
            &self,
            out: *mut *mut AnyObject,
            text: &NSString,
            _range: *mut objc2_foundation::NSRange,
            error: *mut *mut NSError,
        ) -> bool {
            let text = text.to_string();
            let parsed = self.state().parse(&text);
            match parsed {
                Some(seconds) => {
                    if !out.is_null() {
                        let date: Retained<AnyObject> = NSDate::dateWithTimeIntervalSinceReferenceDate(seconds).into();
                        // SAFETY: the caller's storage.
                        unsafe { out.write(Retained::autorelease_ptr(date)) };
                    }
                    true
                }
                None => {
                    let message = format!("The value \u{201c}{text}\u{201d} is invalid.");
                    let info = [(&crate::error::DESCRIPTION, NSString::from_str(&message).into())];
                    // SAFETY: the caller passes null or room for an error.
                    unsafe { crate::error::set(error, crate::error::cocoa(2048, &info)) };
                    false
                }
            }
        }

        #[unsafe(method_id(dateFormat))]
        fn date_format(&self) -> Retained<NSString> {
            NSString::from_str(&self.state().pattern())
        }

        #[unsafe(method(setDateFormat:))]
        fn set_date_format(&self, format: Option<&NSString>) {
            self.state().format = Some(format.map(|f| f.to_string()).unwrap_or_default());
        }

        #[unsafe(method(setLocalizedDateFormatFromTemplate:))]
        fn set_localized_date_format_from_template(&self, template: &NSString) {
            self.state().format = Some(date_format::pattern_from_template(&template.to_string()));
        }

        #[unsafe(method(dateStyle))]
        fn date_style(&self) -> Style {
            self.state().date_style
        }

        #[unsafe(method(setDateStyle:))]
        fn set_date_style(&self, style: Style) {
            let mut state = self.state();
            state.date_style = style;
            state.format = None;
        }

        #[unsafe(method(timeStyle))]
        fn time_style(&self) -> Style {
            self.state().time_style
        }

        #[unsafe(method(setTimeStyle:))]
        fn set_time_style(&self, style: Style) {
            let mut state = self.state();
            state.time_style = style;
            state.format = None;
        }

        #[unsafe(method_id(locale))]
        fn locale(&self) -> Retained<NSLocale> {
            let locale = self.state().locale.clone();
            locale.unwrap_or_else(|| crate::locale::object(&crate::locale::current_identifier()))
        }

        #[unsafe(method(setLocale:))]
        fn set_locale(&self, locale: Option<&NSLocale>) {
            self.state().locale = locale.map(|l| l.retain());
        }

        #[unsafe(method_id(timeZone))]
        fn time_zone(&self) -> Retained<NSTimeZone> {
            let zone = self.state().time_zone.clone();
            zone.unwrap_or_else(|| crate::time_zone::object(crate::time_zone::default_zone()))
        }

        #[unsafe(method(setTimeZone:))]
        fn set_time_zone(&self, zone: Option<&NSTimeZone>) {
            self.state().time_zone = zone.map(|z| z.retain());
        }

        #[unsafe(method(isLenient))]
        fn is_lenient(&self) -> bool {
            self.state().lenient
        }

        #[unsafe(method(setLenient:))]
        fn set_lenient(&self, lenient: bool) {
            self.state().lenient = lenient;
        }

        #[unsafe(method(doesRelativeDateFormatting))]
        fn does_relative_date_formatting(&self) -> bool {
            self.state().relative
        }

        #[unsafe(method(setDoesRelativeDateFormatting:))]
        fn set_does_relative_date_formatting(&self, relative: bool) {
            self.state().relative = relative;
        }

        #[unsafe(method(generatesCalendarDates))]
        fn generates_calendar_dates(&self) -> bool {
            self.state().generates_calendar_dates
        }

        #[unsafe(method(setGeneratesCalendarDates:))]
        fn set_generates_calendar_dates(&self, value: bool) {
            self.state().generates_calendar_dates = value;
        }

        #[unsafe(method(formatterBehavior))]
        fn formatter_behavior(&self) -> NSUInteger {
            self.state().behavior
        }

        #[unsafe(method(setFormatterBehavior:))]
        fn set_formatter_behavior(&self, behavior: NSUInteger) {
            self.state().behavior = behavior;
        }

        #[unsafe(method(formattingContext))]
        fn formatting_context(&self) -> NSInteger {
            self.state().formatting_context
        }

        #[unsafe(method(setFormattingContext:))]
        fn set_formatting_context(&self, context: NSInteger) {
            self.state().formatting_context = context;
        }

        #[unsafe(method_id(twoDigitStartDate))]
        fn two_digit_start_date(&self) -> Option<Retained<NSDate>> {
            let year = self.state().two_digit_start;
            let days = date_format::days_from_civil(year, 1, 1);
            Some(NSDate::dateWithTimeIntervalSinceReferenceDate((days * 86_400) as f64 - crate::date::UNIX_TO_REFERENCE))
        }

        #[unsafe(method(setTwoDigitStartDate:))]
        fn set_two_digit_start_date(&self, date: Option<&NSDate>) {
            let year = date.map_or(1950, |d| date_format::local(crate::date::time_of(d), 0).year);
            self.state().two_digit_start = year;
        }

        #[unsafe(method_id(defaultDate))]
        fn default_date(&self) -> Option<Retained<NSDate>> {
            self.state().default_date.map(NSDate::dateWithTimeIntervalSinceReferenceDate)
        }

        #[unsafe(method(setDefaultDate:))]
        fn set_default_date(&self, date: Option<&NSDate>) {
            self.state().default_date = date.map(crate::date::time_of);
        }

        #[unsafe(method_id(calendar))]
        fn calendar(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setCalendar:))]
        fn set_calendar(&self, _calendar: Option<&AnyObject>) {}

        #[unsafe(method_id(AMSymbol))]
        fn am_symbol(&self) -> Retained<NSString> {
            NSString::from_str(&self.state().symbols.am)
        }

        #[unsafe(method(setAMSymbol:))]
        fn set_am_symbol(&self, symbol: Option<&NSString>) {
            self.state().symbols.am = symbol.map_or_else(|| "AM".into(), |s| s.to_string());
        }

        #[unsafe(method_id(PMSymbol))]
        fn pm_symbol(&self) -> Retained<NSString> {
            NSString::from_str(&self.state().symbols.pm)
        }

        #[unsafe(method(setPMSymbol:))]
        fn set_pm_symbol(&self, symbol: Option<&NSString>) {
            self.state().symbols.pm = symbol.map_or_else(|| "PM".into(), |s| s.to_string());
        }

        #[unsafe(method(setMonthSymbols:))]
        fn set_month_symbols(&self, symbols: Option<&AnyObject>) {
            let list = string_list(symbols, 12).unwrap_or_else(|| Symbols::default().months);
            self.state().symbols.months = list;
        }

        #[unsafe(method(setShortMonthSymbols:))]
        fn set_short_month_symbols(&self, symbols: Option<&AnyObject>) {
            let list = string_list(symbols, 12).unwrap_or_else(|| Symbols::default().short_months);
            self.state().symbols.short_months = list;
        }

        #[unsafe(method(setWeekdaySymbols:))]
        fn set_weekday_symbols(&self, symbols: Option<&AnyObject>) {
            let list = string_list(symbols, 7).unwrap_or_else(|| Symbols::default().weekdays);
            self.state().symbols.weekdays = list;
        }

        #[unsafe(method(setShortWeekdaySymbols:))]
        fn set_short_weekday_symbols(&self, symbols: Option<&AnyObject>) {
            let list = string_list(symbols, 7).unwrap_or_else(|| Symbols::default().short_weekdays);
            self.state().symbols.short_weekdays = list;
        }

        #[unsafe(method_id(monthSymbols))]
        fn month_symbols(&self) -> Retained<AnyObject> {
            array(&self.state().symbols.months)
        }

        #[unsafe(method_id(shortMonthSymbols))]
        fn short_month_symbols(&self) -> Retained<AnyObject> {
            array(&self.state().symbols.short_months)
        }

        #[unsafe(method_id(weekdaySymbols))]
        fn weekday_symbols(&self) -> Retained<AnyObject> {
            array(&self.state().symbols.weekdays)
        }

        #[unsafe(method_id(shortWeekdaySymbols))]
        fn short_weekday_symbols(&self) -> Retained<AnyObject> {
            array(&self.state().symbols.short_weekdays)
        }

        #[unsafe(method_id(eraSymbols))]
        fn era_symbols(&self) -> Retained<AnyObject> {
            array(&self.state().symbols.eras)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            let state = self.state().clone();
            init_state(Self::alloc(), state)
        }
    }
);

impl NSDateFormatterImpl {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        crate::thread::lock(&self.ivars().state)
    }

    /// # Safety
    ///
    /// `out` and `error` are null or writable.
    unsafe fn parse_into(&self, out: *mut *mut AnyObject, text: &str, error: *mut *mut NSString) -> bool {
        match self.state().parse(text) {
            Some(seconds) => {
                if !out.is_null() {
                    let date: Retained<AnyObject> = NSDate::dateWithTimeIntervalSinceReferenceDate(seconds).into();
                    // SAFETY: the caller's storage.
                    unsafe { out.write(Retained::autorelease_ptr(date)) };
                }
                true
            }
            None => {
                if !error.is_null() {
                    // SAFETY: the caller's storage.
                    unsafe { error.write(Retained::autorelease_ptr(NSString::from_str("Error"))) };
                }
                false
            }
        }
    }
}

fn init_state(this: Allocated<NSDateFormatterImpl>, state: State) -> Retained<NSDateFormatterImpl> {
    let this = this.set_ivars(FormatterIvars { state: Mutex::new(state) });
    // SAFETY: NSObject's designated initializer, through NSFormatter.
    unsafe { msg_send![super(this), init] }
}

/// The strings of an array of strings of a given length.
fn string_list(list: Option<&AnyObject>, len: usize) -> Option<Vec<String>> {
    let list = list?;
    // SAFETY: an array; -count takes nothing.
    let count: NSUInteger = unsafe { msg_send![list, count] };
    if count != len {
        return None;
    }
    (0..count)
        .map(|i| {
            // SAFETY: `i` is in bounds.
            let item: Retained<AnyObject> = unsafe { msg_send![list, objectAtIndex: i] };
            item.downcast_ref::<NSString>().map(|s| s.to_string())
        })
        .collect()
}

fn array(list: &[String]) -> Retained<AnyObject> {
    let strings: Vec<Retained<NSString>> = list.iter().map(|s| NSString::from_str(s)).collect();
    objc2_foundation::NSArray::from_retained_slice(&strings).into()
}

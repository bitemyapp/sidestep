//! `CFCalendar`: the Gregorian calendar (and ISO 8601's, the Gregorian
//! with weeks starting on Monday, the first having four days or more) in a
//! time zone, with a locale that sets the first day of the week and the
//! fewest days the first week of a year or month has (`locale_data`'s week
//! data, by region).
//!
//! Sidestep has no `NSCalendar`, so calendars are objects of a private
//! class. The other calendars (Buddhist, Hebrew, Islamic, Japanese,
//! Chinese, …) need data and arithmetic Sidestep doesn't have:
//! `CFCalendarCreateWithIdentifier` returns NULL for them, as for an
//! identifier it doesn't know.
//!
//! The ranges and ordinalities follow what macOS gives (measured with
//! `conformance/tests/cf_types.rs`), including the pairs of units it has
//! no answer for (a day in a week: `{kCFNotFound, kCFNotFound}` and -1).

use std::ffi::c_void;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSLocale, NSString, NSTimeZone};

use super::string::CFRange;
use super::types::{CFTypeID, id, object, owned};

type CFIndex = isize;
type Boolean = u8;

/// `CFCalendarUnit`s.
mod unit {
    pub(super) const ERA: usize = 1 << 1;
    pub(super) const YEAR: usize = 1 << 2;
    pub(super) const MONTH: usize = 1 << 3;
    pub(super) const DAY: usize = 1 << 4;
    pub(super) const HOUR: usize = 1 << 5;
    pub(super) const MINUTE: usize = 1 << 6;
    pub(super) const SECOND: usize = 1 << 7;
    pub(super) const WEEK: usize = 1 << 8;
    pub(super) const WEEKDAY: usize = 1 << 9;
    pub(super) const WEEKDAY_ORDINAL: usize = 1 << 10;
    pub(super) const QUARTER: usize = 1 << 11;
    pub(super) const WEEK_OF_MONTH: usize = 1 << 12;
    pub(super) const WEEK_OF_YEAR: usize = 1 << 13;
    pub(super) const YEAR_FOR_WEEK_OF_YEAR: usize = 1 << 14;
    pub(super) const DAY_OF_YEAR: usize = 1 << 16;
}

/// A calendar's settings.
pub(crate) struct Settings {
    identifier: &'static str,
    locale: Retained<NSLocale>,
    zone: Retained<NSTimeZone>,
    first_weekday: i64,
    min_days: i64,
}

define_class!(
    /// A `CFCalendar`.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCFCalendar"]
    #[ivars = Mutex<Settings>]
    pub(crate) struct Calendar;
);

/// The first weekday and the first week's fewest days for a locale: its
/// region's, or for a language alone, the region it is most spoken in
/// (its own code, or the few exceptions below); Sunday and 1 without one.
pub(crate) fn week_data(identifier: &str) -> (i64, i64) {
    let p = crate::locale::parts(identifier);
    let region = match p.region {
        Some(region) => region.to_ascii_uppercase(),
        None => {
            let language = p.language.to_ascii_lowercase();
            let exception = match language.as_str() {
                "en" => "US",
                "ar" => "EG",
                "he" | "iw" => "IL",
                "ja" => "JP",
                "zh" => "CN",
                "ko" => "KR",
                "sv" => "SE",
                "da" => "DK",
                "nb" | "nn" | "no" => "NO",
                "el" => "GR",
                "cs" => "CZ",
                "uk" => "UA",
                "hi" => "IN",
                "fa" => "IR",
                "vi" => "VN",
                "sl" => "SI",
                "et" => "EE",
                "ga" => "IE",
                "hy" => "AM",
                "ka" => "GE",
                "kk" => "KZ",
                "ur" => "PK",
                _ => "",
            };
            if exception.is_empty() { language.to_ascii_uppercase() } else { exception.to_string() }
        }
    };
    for &(first, min, regions) in super::locale_data::WEEKS {
        if regions.split(' ').any(|r| r == region) {
            return (i64::from(first), i64::from(min));
        }
    }
    let known = super::locale_data::ISO_COUNTRIES.contains(&region.as_str());
    if known { (2, 1) } else { (1, 1) }
}

/// A new calendar, if Sidestep has one by that name.
pub(crate) fn make(identifier: &str, locale: Retained<NSLocale>) -> Option<Retained<Calendar>> {
    let (identifier, iso) = match identifier {
        "gregorian" => ("gregorian", false),
        "iso8601" => ("iso8601", true),
        _ => return None,
    };
    let (first_weekday, min_days) = if iso { (2, 4) } else { week_data(crate::locale::identifier_of(&locale)) };
    let zone = crate::time_zone::object(crate::time_zone::default_zone());
    let settings = Settings { identifier, locale, zone, first_weekday, min_days };
    let this = Calendar::alloc().set_ivars(Mutex::new(settings));
    // SAFETY: NSObject's initializer.
    Some(unsafe { msg_send![super(this), init] })
}

fn settings<'a>(cf: *const c_void) -> std::sync::MutexGuard<'a, Settings> {
    // SAFETY: the callers' contracts: `cf` is a calendar from `make`, live
    // while they use it.
    let calendar: &'a Calendar = unsafe { &*cf.cast::<Calendar>() };
    crate::thread::lock(calendar.ivars())
}

macro_rules! identifiers {
    ($($name:ident = $value:literal,)*) => {
        $(crate::constant_string!($name = $value);)*
    };
}

identifiers! {
    kCFGregorianCalendar = "gregorian",
    kCFBuddhistCalendar = "buddhist",
    kCFChineseCalendar = "chinese",
    kCFHebrewCalendar = "hebrew",
    kCFIslamicCalendar = "islamic",
    kCFIslamicCivilCalendar = "islamic-civil",
    kCFJapaneseCalendar = "japanese",
    kCFRepublicOfChinaCalendar = "roc",
    kCFPersianCalendar = "persian",
    kCFIndianCalendar = "indian",
    kCFISO8601Calendar = "iso8601",
    kCFIslamicTabularCalendar = "islamic-tbla",
    kCFIslamicUmmAlQuraCalendar = "islamic-umalqura",
    kCFBanglaCalendar = "bangla",
    kCFGujaratiCalendar = "gujarati",
    kCFKannadaCalendar = "kannada",
    kCFMalayalamCalendar = "malayalam",
    kCFMarathiCalendar = "marathi",
    kCFOdiaCalendar = "odia",
    kCFTamilCalendar = "tamil",
    kCFTeluguCalendar = "telugu",
    kCFVikramCalendar = "vikram",
    kCFDangiCalendar = "dangi",
    kCFVietnameseCalendar = "vietnamese",
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFCalendarGetTypeID() -> CFTypeID {
    id::CALENDAR
}

/// The current locale's calendar in the default time zone.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFCalendarCopyCurrent() -> *mut c_void {
    make("gregorian", NSLocale::currentLocale()).map_or(std::ptr::null_mut(), owned)
}

/// A calendar in the system locale and the default time zone; NULL for
/// one Sidestep doesn't have.
///
/// # Safety
///
/// `identifier` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarCreateWithIdentifier(
    _alloc: *const c_void,
    identifier: *const c_void,
) -> *mut c_void {
    if identifier.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let identifier = super::string::text(unsafe { object(identifier) }).into_owned();
    make(&identifier, crate::locale::object("")).map_or(std::ptr::null_mut(), owned)
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetIdentifier(cf: *const c_void) -> *const c_void {
    static GREGORIAN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static ISO: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let (slot, name) = match settings(cf).identifier {
        "iso8601" => (&ISO, "iso8601"),
        _ => (&GREGORIAN, "gregorian"),
    };
    // The identifiers are constants, made once.
    *slot.get_or_init(|| Retained::into_raw(NSString::from_str(name)) as usize) as *const c_void
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarCopyLocale(cf: *const c_void) -> *mut c_void {
    owned(settings(cf).locale.clone())
}

/// Set the locale, and the week data that comes with it (an ISO 8601
/// calendar keeps its own).
///
/// # Safety
///
/// `cf` is a calendar; `locale` null or a locale.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarSetLocale(cf: *const c_void, locale: *const c_void) {
    if locale.is_null() {
        return;
    }
    // SAFETY: per this function's contract.
    let locale = unsafe { &*locale.cast::<NSLocale>() }.retain();
    let mut s = settings(cf);
    if s.identifier != "iso8601" {
        (s.first_weekday, s.min_days) = week_data(crate::locale::identifier_of(&locale));
    }
    s.locale = locale;
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarCopyTimeZone(cf: *const c_void) -> *mut c_void {
    owned(settings(cf).zone.clone())
}

/// # Safety
///
/// `cf` is a calendar; `zone` null or a time zone.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarSetTimeZone(cf: *const c_void, zone: *const c_void) {
    if !zone.is_null() {
        // SAFETY: per this function's contract.
        settings(cf).zone = unsafe { &*zone.cast::<NSTimeZone>() }.retain();
    }
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetFirstWeekday(cf: *const c_void) -> CFIndex {
    settings(cf).first_weekday as CFIndex
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarSetFirstWeekday(cf: *const c_void, weekday: CFIndex) {
    if (1..=7).contains(&weekday) {
        settings(cf).first_weekday = weekday as i64;
    }
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetMinimumDaysInFirstWeek(cf: *const c_void) -> CFIndex {
    settings(cf).min_days as CFIndex
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarSetMinimumDaysInFirstWeek(cf: *const c_void, days: CFIndex) {
    if (1..=7).contains(&days) {
        settings(cf).min_days = days as i64;
    }
}

const NONE: CFRange = CFRange { location: -1, length: -1 };

fn range(location: i64, length: i64) -> CFRange {
    CFRange { location: location as CFIndex, length: length as CFIndex }
}

/// The smallest and largest ranges a unit's values take in the Gregorian
/// calendar (its week units as macOS gives them for weeks starting on
/// Sunday with a one-day first week).
fn limits(unit: usize) -> Option<(CFRange, CFRange)> {
    use unit::*;
    let pair = |a: (i64, i64), b: (i64, i64)| Some((range(a.0, a.1), range(b.0, b.1)));
    match unit {
        ERA => pair((0, 2), (0, 2)),
        YEAR => pair((1, 140_742), (1, 144_683)),
        MONTH | QUARTER => {
            let n = if unit == MONTH { 12 } else { 4 };
            pair((1, n), (1, n))
        }
        DAY => pair((1, 28), (1, 31)),
        HOUR => pair((0, 24), (0, 24)),
        MINUTE | SECOND => pair((0, 60), (0, 60)),
        WEEK | WEEK_OF_YEAR => pair((1, 52), (1, 53)),
        WEEKDAY => pair((1, 7), (1, 7)),
        WEEKDAY_ORDINAL => pair((1, 4), (1, 5)),
        WEEK_OF_MONTH => pair((1, 4), (1, 6)),
        YEAR_FOR_WEEK_OF_YEAR => pair((140_742, 1), (140_742, 3942)),
        DAY_OF_YEAR => pair((1, 365), (1, 366)),
        _ => None,
    }
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetMinimumRangeOfUnit(_cf: *const c_void, unit: usize) -> CFRange {
    limits(unit).map_or(NONE, |l| l.0)
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetMaximumRangeOfUnit(_cf: *const c_void, unit: usize) -> CFRange {
    limits(unit).map_or(NONE, |l| l.1)
}

// Civil dates: days counted from 1970-01-01 (proleptic Gregorian).

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970 to the reference date, 2001-01-01.
const REFERENCE_DAY: i64 = 11_323;
/// Days from 1970 to 0001-01-01.
const ERA_DAY: i64 = -719_162;

/// 1 for Sunday to 7 for Saturday.
fn weekday(day: i64) -> i64 {
    (day + 4).rem_euclid(7) + 1
}

/// A moment's local date and time in a calendar.
#[derive(Clone, Copy)]
struct Local {
    /// Days since 1970, local.
    day: i64,
    /// Seconds into the day.
    seconds: f64,
    year: i64,
    month: i64,
    date: i64,
    first_weekday: i64,
    min_days: i64,
}

impl Local {
    fn hour(&self) -> i64 {
        (self.seconds / 3600.0).floor() as i64
    }

    fn minute(&self) -> i64 {
        (self.seconds / 60.0).floor() as i64 % 60
    }

    fn second(&self) -> i64 {
        self.seconds.floor() as i64 % 60
    }

    /// Days before this one in its week (0 to 6).
    fn in_week(&self) -> i64 {
        (weekday(self.day) - self.first_weekday).rem_euclid(7)
    }

    fn day_of_year(&self) -> i64 {
        self.day - days_from_civil(self.year, 1, 1) + 1
    }

    fn quarter(&self) -> i64 {
        (self.month - 1) / 3 + 1
    }

    fn quarter_start(&self) -> i64 {
        days_from_civil(self.year, (self.quarter() - 1) * 3 + 1, 1)
    }

    /// The first day of the first week of the span starting on `first`:
    /// the week holding at least `min_days` of it.
    fn first_week_start(&self, first: i64) -> i64 {
        let offset = (weekday(first) - self.first_weekday).rem_euclid(7);
        if 7 - offset >= self.min_days { first - offset } else { first - offset + 7 }
    }

    /// The year the week is counted in and the first day of its first week.
    fn week_year(&self) -> (i64, i64) {
        let this = self.first_week_start(days_from_civil(self.year, 1, 1));
        let next = self.first_week_start(days_from_civil(self.year + 1, 1, 1));
        if self.day >= next {
            (self.year + 1, next)
        } else if self.day < this {
            (self.year - 1, self.first_week_start(days_from_civil(self.year - 1, 1, 1)))
        } else {
            (self.year, this)
        }
    }

    fn week_of_year(&self) -> i64 {
        (self.day - self.week_year().1).div_euclid(7) + 1
    }

    fn week_of_month(&self) -> i64 {
        (self.day - self.first_week_start(days_from_civil(self.year, self.month, 1))).div_euclid(7) + 1
    }

    /// Weeks from the week holding `first` to this day's: 1 for that week.
    fn weeks_since(&self, first: i64) -> i64 {
        let offset = (weekday(first) - self.first_weekday).rem_euclid(7);
        (self.day - first + offset).div_euclid(7) + 1
    }

    fn days_in_year(&self) -> i64 {
        if is_leap(self.year) { 366 } else { 365 }
    }
}

/// The local date and time at `at` in the calendar's zone.
fn local(s: &Settings, at: f64) -> Local {
    let offset = f64::from(crate::time_zone::zone_of(&s.zone).at(at).offset);
    let local = at + offset;
    let days = (local / 86_400.0).floor();
    let day = days as i64 + REFERENCE_DAY;
    let (year, month, date) = civil_from_days(day);
    Local {
        day,
        seconds: local - days * 86_400.0,
        year,
        month,
        date,
        first_weekday: s.first_weekday,
        min_days: s.min_days,
    }
}

/// The absolute time of a local day's midnight.
fn midnight(s: &Settings, day: i64) -> f64 {
    let wall = (day - REFERENCE_DAY) as f64 * 86_400.0;
    let zone = crate::time_zone::zone_of(&s.zone);
    let guess = wall - f64::from(zone.at(wall).offset);
    wall - f64::from(zone.at(guess).offset)
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetOrdinalityOfUnit(
    cf: *const c_void,
    smaller: usize,
    bigger: usize,
    at: f64,
) -> CFIndex {
    let s = settings(cf);
    let l = local(&s, at);
    ordinality(&l, smaller, bigger).unwrap_or(-1) as CFIndex
}

fn ordinality(l: &Local, smaller: usize, bigger: usize) -> Option<i64> {
    use unit::*;
    let era_days = l.day - ERA_DAY;
    let (week_year, week_year_start) = l.week_year();
    // Days before this one in each bigger unit.
    let days_before = |bigger: usize| -> Option<i64> {
        Some(match bigger {
            ERA => era_days,
            YEAR => l.day_of_year() - 1,
            MONTH => l.date - 1,
            DAY | WEEKDAY | DAY_OF_YEAR => 0,
            WEEK | WEEK_OF_MONTH | WEEK_OF_YEAR => l.in_week(),
            QUARTER => l.day - l.quarter_start(),
            YEAR_FOR_WEEK_OF_YEAR => l.day - week_year_start,
            _ => return None,
        })
    };
    let week_in = |bigger: usize| -> Option<i64> {
        Some(match bigger {
            ERA => l.weeks_since(l.day - era_days),
            YEAR | YEAR_FOR_WEEK_OF_YEAR => l.week_of_year(),
            QUARTER => l.weeks_since(l.quarter_start()),
            _ => return None,
        })
    };
    Some(match smaller {
        ERA => return None,
        YEAR => match bigger {
            ERA => l.year,
            _ => return None,
        },
        YEAR_FOR_WEEK_OF_YEAR => match bigger {
            ERA => week_year,
            _ => return None,
        },
        QUARTER => match bigger {
            ERA => (l.year - 1) * 4 + l.quarter(),
            YEAR => l.quarter(),
            _ => return None,
        },
        MONTH => match bigger {
            ERA => (l.year - 1) * 12 + l.month,
            YEAR => l.month,
            QUARTER => (l.month - 1) % 3 + 1,
            _ => return None,
        },
        WEEK | WEEK_OF_YEAR => week_in(bigger)?,
        WEEK_OF_MONTH => match bigger {
            MONTH => l.week_of_month(),
            _ => week_in(bigger)?,
        },
        WEEKDAY | WEEKDAY_ORDINAL => match bigger {
            ERA => week_in(ERA)?,
            WEEK | WEEK_OF_MONTH | WEEK_OF_YEAR if smaller == WEEKDAY => l.in_week() + 1,
            MONTH => (l.date - 1) / 7 + 1,
            YEAR | QUARTER | YEAR_FOR_WEEK_OF_YEAR => days_before(bigger)? / 7 + 1,
            _ => return None,
        },
        DAY => match bigger {
            DAY | WEEKDAY | DAY_OF_YEAR => return None,
            _ => days_before(bigger)? + 1,
        },
        DAY_OF_YEAR => match bigger {
            YEAR | QUARTER | YEAR_FOR_WEEK_OF_YEAR => days_before(bigger)? + 1,
            _ => return None,
        },
        // In a week-numbering year, macOS counts whole days only: the time
        // of day doesn't count.
        HOUR | MINUTE | SECOND if bigger == YEAR_FOR_WEEK_OF_YEAR => {
            let per_day = match smaller {
                HOUR => 24,
                MINUTE => 1440,
                _ => 86_400,
            };
            days_before(bigger)? * per_day + 1
        }
        HOUR => days_before(bigger).map(|d| d * 24 + l.hour() + 1)?,
        MINUTE => match bigger {
            HOUR => l.minute() + 1,
            _ => days_before(bigger).map(|d| (d * 24 + l.hour()) * 60 + l.minute() + 1)?,
        },
        SECOND => match bigger {
            MINUTE => l.second() + 1,
            HOUR => l.minute() * 60 + l.second() + 1,
            _ => days_before(bigger).map(|d| ((d * 24 + l.hour()) * 60 + l.minute()) * 60 + l.second() + 1)?,
        },
        _ => return None,
    })
}

/// # Safety
///
/// `cf` is a calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetRangeOfUnit(
    cf: *const c_void,
    smaller: usize,
    bigger: usize,
    at: f64,
) -> CFRange {
    let s = settings(cf);
    let l = local(&s, at);
    range_in(&l, smaller, bigger).unwrap_or(NONE)
}

fn range_in(l: &Local, smaller: usize, bigger: usize) -> Option<CFRange> {
    use unit::*;
    Some(match (smaller, bigger) {
        (YEAR, ERA) => range(1, 144_683),
        (MONTH, ERA | YEAR) => range(1, 12),
        (MONTH, QUARTER) => range(1, 3),
        (QUARTER, ERA | YEAR) => range(1, 4),
        (DAY, ERA) => range(1, 31),
        (DAY, YEAR) | (DAY_OF_YEAR, YEAR) => range(1, l.days_in_year()),
        (DAY, MONTH) => range(1, days_in_month(l.year, l.month)),
        (DAY, QUARTER) => {
            let start = l.quarter_start();
            let (y, m) = if l.quarter() == 4 { (l.year + 1, 1) } else { (l.year, l.quarter() * 3 + 1) };
            range(1, days_from_civil(y, m, 1) - start)
        }
        // The days of the month the week holds.
        (DAY, WEEK_OF_MONTH) => {
            let week_start = l.day - l.in_week();
            let month_start = days_from_civil(l.year, l.month, 1);
            let month_end = month_start + days_in_month(l.year, l.month);
            let first = week_start.max(month_start);
            let last = (week_start + 7).min(month_end);
            range(first - month_start + 1, last - first)
        }
        (HOUR, b) if b != HOUR && b != MINUTE && b != SECOND => range(0, 24),
        (MINUTE, b) if b != MINUTE && b != SECOND => range(0, 60),
        (SECOND, b) if b != SECOND => range(0, 60),
        (WEEK | WEEK_OF_YEAR, ERA) => range(1, 53),
        (WEEK | WEEK_OF_YEAR, YEAR) => {
            // The number the year's last day's week would have, counted on
            // from its first week, as macOS counts.
            let this = l.first_week_start(days_from_civil(l.year, 1, 1));
            let last_day = days_from_civil(l.year + 1, 1, 1) - 1;
            range(1, (last_day - this).div_euclid(7) + 1)
        }
        (WEEK | WEEK_OF_YEAR, MONTH) => {
            // The numbers (in the year) of the weeks the month spans.
            let first = days_from_civil(l.year, l.month, 1);
            let last = first + days_in_month(l.year, l.month) - 1;
            let at = |day: i64| {
                let (year, month, date) = civil_from_days(day);
                Local { day, seconds: 0.0, year, month, date, first_weekday: l.first_weekday, min_days: l.min_days }
                    .week_of_year()
            };
            let (a, b) = (at(first), at(last));
            if b >= a { range(a, b - a + 1) } else { range(a, 1) }
        }
        (WEEK | WEEK_OF_YEAR, QUARTER) => {
            let start = l.quarter_start();
            let (y, m) = if l.quarter() == 4 { (l.year + 1, 1) } else { (l.year, l.quarter() * 3 + 1) };
            let last = days_from_civil(y, m, 1) - 1;
            let last_local = Local { day: last, ..*l };
            range(1, last_local.weeks_since(start))
        }
        (WEEK | WEEK_OF_YEAR, YEAR_FOR_WEEK_OF_YEAR) => {
            let (year, start) = l.week_year();
            let next = l.first_week_start(days_from_civil(year + 1, 1, 1));
            range(1, (next - start) / 7)
        }
        (WEEKDAY, b) if b != WEEKDAY && b != DAY && b != HOUR && b != MINUTE && b != SECOND => range(1, 7),
        (WEEKDAY_ORDINAL, ERA) => range(1, 5),
        (WEEKDAY_ORDINAL, MONTH) => range(1, (days_in_month(l.year, l.month) + 6) / 7),
        (WEEK_OF_MONTH, ERA) => range(1, 6),
        (WEEK_OF_MONTH, MONTH) => {
            let first = days_from_civil(l.year, l.month, 1);
            let last = Local { day: first + days_in_month(l.year, l.month) - 1, ..*l };
            range(1, last.week_of_month())
        }
        _ => return None,
    })
}

/// Where the unit holding `at` starts and how long it lasts.
///
/// # Safety
///
/// `cf` is a calendar; `start` and `length` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCalendarGetTimeRangeOfUnit(
    cf: *const c_void,
    unit: usize,
    at: f64,
    start: *mut f64,
    length: *mut f64,
) -> Boolean {
    use unit::*;
    let s = settings(cf);
    let l = local(&s, at);
    let span = |from: i64, to: i64| (midnight(&s, from), midnight(&s, to) - midnight(&s, from));
    let (begin, len) = match unit {
        ERA => (midnight(&s, ERA_DAY), 4_398_046_511_104.0),
        YEAR => span(days_from_civil(l.year, 1, 1), days_from_civil(l.year + 1, 1, 1)),
        QUARTER => {
            let (y, m) = if l.quarter() == 4 { (l.year + 1, 1) } else { (l.year, l.quarter() * 3 + 1) };
            span(l.quarter_start(), days_from_civil(y, m, 1))
        }
        MONTH => {
            let (y, m) = if l.month == 12 { (l.year + 1, 1) } else { (l.year, l.month + 1) };
            span(days_from_civil(l.year, l.month, 1), days_from_civil(y, m, 1))
        }
        WEEK | WEEK_OF_MONTH | WEEK_OF_YEAR => {
            let first = l.day - l.in_week();
            span(first, first + 7)
        }
        YEAR_FOR_WEEK_OF_YEAR => {
            let (year, first) = l.week_year();
            span(first, l.first_week_start(days_from_civil(year + 1, 1, 1)))
        }
        DAY | WEEKDAY | WEEKDAY_ORDINAL | DAY_OF_YEAR => span(l.day, l.day + 1),
        HOUR | MINUTE | SECOND => {
            let size = match unit {
                HOUR => 3600.0,
                MINUTE => 60.0,
                _ => 1.0,
            };
            (at - l.seconds % size, size)
        }
        _ => return 0,
    };
    // SAFETY: per this function's contract.
    unsafe {
        if !start.is_null() {
            start.write(begin);
        }
        if !length.is_null() {
            length.write(len);
        }
    }
    1
}

/// A calendar for a locale's `kCFLocaleCalendarKey`.
pub(crate) fn for_locale(locale: &NSLocale, identifier: &str) -> Option<Retained<AnyObject>> {
    make(identifier, locale.retain()).map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2001, 1, 1), REFERENCE_DAY);
        assert_eq!(days_from_civil(1, 1, 1), ERA_DAY);
        assert_eq!(civil_from_days(days_from_civil(2024, 3, 15)), (2024, 3, 15));
        assert_eq!(weekday(days_from_civil(2024, 3, 15)), 6);
        assert_eq!(weekday(ERA_DAY), 2);
    }

    fn at(first_weekday: i64, min_days: i64) -> Local {
        let day = days_from_civil(2024, 3, 15);
        Local {
            day,
            seconds: 13.0 * 3600.0 + 45.0 * 60.0 + 30.0,
            year: 2024,
            month: 3,
            date: 15,
            first_weekday,
            min_days,
        }
    }

    #[test]
    fn ordinalities_as_measured() {
        use unit::*;
        let l = at(1, 1);
        let cases = [
            (YEAR, ERA, 2024),
            (MONTH, ERA, 24_279),
            (MONTH, QUARTER, 3),
            (DAY, ERA, 738_960),
            (DAY, YEAR, 75),
            (DAY, WEEK, 6),
            (DAY, YEAR_FOR_WEEK_OF_YEAR, 76),
            (HOUR, ERA, 17_735_030),
            (HOUR, WEEK, 134),
            (MINUTE, ERA, 1_064_101_786),
            (SECOND, ERA, 63_846_107_131),
            (SECOND, YEAR_FOR_WEEK_OF_YEAR, 6_480_001),
            (WEEK, ERA, 105_566),
            (WEEK, YEAR, 11),
            (WEEK, QUARTER, 11),
            (WEEKDAY, YEAR, 11),
            (WEEKDAY, MONTH, 3),
            (WEEKDAY, WEEK, 6),
            (WEEKDAY, ERA, 105_566),
            (QUARTER, ERA, 8093),
            (WEEK_OF_MONTH, MONTH, 3),
            (YEAR_FOR_WEEK_OF_YEAR, ERA, 2024),
            (DAY_OF_YEAR, YEAR_FOR_WEEK_OF_YEAR, 76),
        ];
        for (s, b, want) in cases {
            assert_eq!(ordinality(&l, s, b), Some(want), "{s} in {b}");
        }
        assert_eq!(ordinality(&l, WEEK, MONTH), None);
        assert_eq!(ordinality(&l, WEEKDAY, DAY_OF_YEAR), None);
        let ranges = [
            (DAY, MONTH, (1, 31)),
            (DAY, YEAR, (1, 366)),
            (DAY, QUARTER, (1, 91)),
            (DAY, WEEK_OF_MONTH, (10, 7)),
            (WEEK, YEAR, (1, 53)),
            (WEEK, MONTH, (9, 6)),
            (WEEK, QUARTER, (1, 14)),
            (WEEK, YEAR_FOR_WEEK_OF_YEAR, (1, 52)),
            (WEEKDAY_ORDINAL, MONTH, (1, 5)),
            (WEEK_OF_MONTH, MONTH, (1, 6)),
        ];
        for (s, b, want) in ranges {
            let r = range_in(&l, s, b).unwrap_or(NONE);
            assert_eq!((r.location as i64, r.length as i64), want, "{s} in {b}");
        }
        assert!(range_in(&l, DAY, WEEK).is_none());
    }
}

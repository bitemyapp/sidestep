//! `NSDate`, `NSDateFormatter`, `NSLocale` and `NSTimeZone`, checked on
//! macOS and on Linux alike.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, msg_send};
use objc2_core_foundation::CFAbsoluteTimeGetCurrent;
use objc2_foundation::{
    NSComparisonResult, NSDate, NSDateFormatter, NSDateFormatterStyle, NSLocale, NSObjectProtocol, NSString, NSTimeZone,
};

use sidestep as _;

fn at(seconds: f64) -> Retained<NSDate> {
    NSDate::dateWithTimeIntervalSinceReferenceDate(seconds)
}

#[test]
fn reference_date_and_constants() {
    assert_eq!(at(0.0).timeIntervalSince1970(), 978_307_200.0);
    assert_eq!(NSDate::dateWithTimeIntervalSince1970(978_307_200.0).timeIntervalSinceReferenceDate(), 0.0);
    assert_eq!(NSDate::distantFuture().timeIntervalSinceReferenceDate(), 63_113_904_000.0);
    assert_eq!(NSDate::distantPast().timeIntervalSinceReferenceDate(), -63_114_076_800.0);
}

#[test]
fn now() {
    let before = CFAbsoluteTimeGetCurrent();
    let now = NSDate::now();
    let date = NSDate::date();
    let new = NSDate::new();
    let class_now = NSDate::timeIntervalSinceReferenceDate_class();
    let after = CFAbsoluteTimeGetCurrent();
    for t in [
        now.timeIntervalSinceReferenceDate(),
        date.timeIntervalSinceReferenceDate(),
        new.timeIntervalSinceReferenceDate(),
        class_now,
    ] {
        assert!(before <= t && t <= after, "{before} <= {t} <= {after}");
    }
    let soon = NSDate::dateWithTimeIntervalSinceNow(10.0).timeIntervalSinceNow();
    assert!((9.9..=10.0).contains(&soon));
    let soon = NSDate::initWithTimeIntervalSinceNow(NSDate::alloc(), -3.0).timeIntervalSinceNow();
    assert!((-3.1..=-3.0).contains(&soon));
}

#[test]
fn arithmetic() {
    let base = at(1000.0);
    assert_eq!(NSDate::dateWithTimeInterval_sinceDate(5.0, &base).timeIntervalSinceReferenceDate(), 1005.0);
    assert_eq!(
        NSDate::initWithTimeInterval_sinceDate(NSDate::alloc(), -5.0, &base).timeIntervalSinceReferenceDate(),
        995.0
    );
    assert_eq!(base.dateByAddingTimeInterval(0.5).timeIntervalSinceReferenceDate(), 1000.5);
    assert_eq!(at(1500.0).timeIntervalSinceDate(&base), 500.0);
    assert_eq!(
        NSDate::initWithTimeIntervalSinceReferenceDate(NSDate::alloc(), 7.0).timeIntervalSinceReferenceDate(),
        7.0
    );
    assert_eq!(
        NSDate::initWithTimeIntervalSince1970(NSDate::alloc(), 0.0).timeIntervalSinceReferenceDate(),
        -978_307_200.0
    );
}

#[test]
fn comparison_is_exact() {
    let a = at(1000.0);
    let b = at(1000.0 + 1e-7);
    let c = at(1000.0);
    assert!(!a.isEqualToDate(&b));
    assert!(!a.isEqual(Some(&b)));
    assert_eq!(a.compare(&b), NSComparisonResult::Ascending);
    assert_eq!(b.compare(&a), NSComparisonResult::Descending);
    assert!(a.isEqualToDate(&c));
    assert!(a.isEqual(Some(&c)));
    assert_eq!(a.hash(), c.hash());
    assert_eq!(a.compare(&c), NSComparisonResult::Same);
    // Ties go to the receiver.
    assert!(std::ptr::eq(&*a.earlierDate(&c), &*a));
    assert!(std::ptr::eq(&*a.laterDate(&c), &*a));
    assert!(std::ptr::eq(&*a.earlierDate(&b), &*a));
    assert!(std::ptr::eq(&*a.laterDate(&b), &*b));
    let other: Retained<AnyObject> = objc2_foundation::NSObject::new().into();
    assert!(!a.isEqual(Some(&other)));
}

#[test]
fn copies_are_the_same_object() {
    let a = at(42.0);
    let copy: Retained<NSDate> = unsafe { msg_send![&*a, copy] };
    assert!(std::ptr::eq(&*copy, &*a));
}

#[test]
fn description() {
    let describe = |t: f64| at(t).description().to_string();
    assert_eq!(describe(0.0), "2001-01-01 00:00:00 +0000");
    // Fractions of a second are cut off, not rounded.
    assert_eq!(describe(123_456_789.987), "2004-11-29 21:33:09 +0000");
    assert_eq!(describe(-1.0e9), "1969-04-24 22:13:20 +0000");
    assert_eq!(describe(-0.5), "2000-12-31 23:59:59 +0000");
    assert_eq!(NSDate::distantFuture().description().to_string(), "4001-01-01 00:00:00 +0000");
    // Before 15 October 1582, dates are named in the Julian calendar.
    assert_eq!(NSDate::distantPast().description().to_string(), "0001-01-01 00:00:00 +0000");
    assert_eq!(
        NSDate::dateWithTimeIntervalSince1970(-12_219_292_800.0).description().to_string(),
        "1582-10-15 00:00:00 +0000"
    );
    assert_eq!(
        NSDate::dateWithTimeIntervalSince1970(-12_219_292_801.0).description().to_string(),
        "1582-10-04 23:59:59 +0000"
    );
}

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// 2025-09-19 18:40:00.5 UTC.
const MOMENT: f64 = 780_000_000.5;

fn formatter(locale: &str, zone: &NSTimeZone) -> Retained<NSDateFormatter> {
    let f = NSDateFormatter::new();
    f.setLocale(Some(&NSLocale::localeWithLocaleIdentifier(&ns(locale))));
    f.setTimeZone(Some(zone));
    f
}

fn zone(name: &str) -> Retained<NSTimeZone> {
    NSTimeZone::timeZoneWithName(&ns(name)).unwrap_or_else(|| panic!("{name} is a zone"))
}

#[test]
fn style_pairs_in_en_us() {
    const STYLES: [NSDateFormatterStyle; 5] = [
        NSDateFormatterStyle::NoStyle,
        NSDateFormatterStyle::ShortStyle,
        NSDateFormatterStyle::MediumStyle,
        NSDateFormatterStyle::LongStyle,
        NSDateFormatterStyle::FullStyle,
    ];
    let expected: [[&str; 5]; 5] = [
        ["", "2:40\u{202f}PM", "2:40:00\u{202f}PM", "2:40:00\u{202f}PM EDT", "2:40:00\u{202f}PM Eastern Daylight Time"],
        [
            "9/19/25",
            "9/19/25, 2:40\u{202f}PM",
            "9/19/25, 2:40:00\u{202f}PM",
            "9/19/25, 2:40:00\u{202f}PM EDT",
            "9/19/25, 2:40:00\u{202f}PM Eastern Daylight Time",
        ],
        [
            "Sep 19, 2025",
            "Sep 19, 2025 at 2:40\u{202f}PM",
            "Sep 19, 2025 at 2:40:00\u{202f}PM",
            "Sep 19, 2025 at 2:40:00\u{202f}PM EDT",
            "Sep 19, 2025 at 2:40:00\u{202f}PM Eastern Daylight Time",
        ],
        [
            "September 19, 2025",
            "September 19, 2025 at 2:40\u{202f}PM",
            "September 19, 2025 at 2:40:00\u{202f}PM",
            "September 19, 2025 at 2:40:00\u{202f}PM EDT",
            "September 19, 2025 at 2:40:00\u{202f}PM Eastern Daylight Time",
        ],
        [
            "Friday, September 19, 2025",
            "Friday, September 19, 2025 at 2:40\u{202f}PM",
            "Friday, September 19, 2025 at 2:40:00\u{202f}PM",
            "Friday, September 19, 2025 at 2:40:00\u{202f}PM EDT",
            "Friday, September 19, 2025 at 2:40:00\u{202f}PM Eastern Daylight Time",
        ],
    ];
    let f = formatter("en_US", &zone("America/New_York"));
    for (d, row) in STYLES.iter().zip(expected) {
        for (t, text) in STYLES.iter().zip(row) {
            f.setDateStyle(*d);
            f.setTimeStyle(*t);
            assert_eq!(f.stringFromDate(&at(MOMENT)).to_string(), text, "{d:?}/{t:?}");
        }
    }
    f.setDateStyle(NSDateFormatterStyle::MediumStyle);
    f.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    assert_eq!(f.dateFormat().to_string(), "MMM d, y 'at' h:mm\u{202f}a");

    let f = formatter("en_US", &NSTimeZone::timeZoneForSecondsFromGMT(0));
    f.setDateStyle(NSDateFormatterStyle::FullStyle);
    f.setTimeStyle(NSDateFormatterStyle::FullStyle);
    assert_eq!(
        f.stringFromDate(&at(MOMENT)).to_string(),
        "Friday, September 19, 2025 at 6:40:00\u{202f}PM Greenwich Mean Time"
    );
    f.setTimeZone(Some(&NSTimeZone::timeZoneForSecondsFromGMT(19800)));
    assert_eq!(
        f.stringFromDate(&at(MOMENT)).to_string(),
        "Saturday, September 20, 2025 at 12:10:00\u{202f}AM GMT+05:30"
    );
    f.setDateStyle(NSDateFormatterStyle::LongStyle);
    f.setTimeStyle(NSDateFormatterStyle::LongStyle);
    assert_eq!(f.stringFromDate(&at(MOMENT)).to_string(), "September 20, 2025 at 12:10:00\u{202f}AM GMT+5:30");

    let fresh = NSDateFormatter::new();
    assert_eq!(fresh.dateFormat().to_string(), "");
    assert_eq!(fresh.stringFromDate(&at(MOMENT)).to_string(), "");
    assert!(!fresh.isLenient());
}

#[test]
fn en_us_posix_patterns_round_trip() {
    let f = formatter("en_US_POSIX", &NSTimeZone::timeZoneForSecondsFromGMT(0));
    for (pattern, text, back) in [
        ("yyyy-MM-dd'T'HH:mm:ssZZZZZ", "2025-09-19T18:40:00Z", Some(780_000_000.0)),
        ("yyyy-MM-dd'T'HH:mm:ss.SSSZ", "2025-09-19T18:40:00.500+0000", Some(MOMENT)),
        ("EEE, dd MMM yyyy HH:mm:ss zzz", "Fri, 19 Sep 2025 18:40:00 GMT", Some(780_000_000.0)),
        ("EEEE MMMM d y G h:mm a", "Friday September 19 2025 AD 6:40 PM", Some(780_000_000.0)),
        ("yy M/d H:m:s", "25 9/19 18:40:0", Some(780_000_000.0)),
        ("''", "'", Some(-31_622_400.0)),
        ("'it''s' h", "it's 6", Some(-31_600_800.0)),
        ("hh:mm aaa K k", "06:40 PM 6 18", Some(-31_555_200.0)),
        ("LLLL LLL", "September Sep", Some(-10_540_800.0)),
        ("yyyyy", "02025", Some(757_382_400.0)),
    ] {
        f.setDateFormat(Some(&ns(pattern)));
        assert_eq!(f.stringFromDate(&at(MOMENT)).to_string(), text, "{pattern:?}");
        assert_eq!(f.dateFromString(&ns(text)).map(|d| d.timeIntervalSinceReferenceDate()), back, "{pattern:?}");
    }
    for (pattern, text) in [
        ("D w W e c Q QQQQ", "262 38 3 6 6 3 3rd quarter"),
        ("G GGGG GGGGG", "AD Anno Domini A"),
        ("q qq qqq qqqq", "3 03 Q3 3rd quarter"),
        ("E EE EEE EEEE EEEEE EEEEEE", "Fri Fri Fri Friday F Fr"),
        ("e ee eee eeee", "6 06 Fri Friday"),
        ("c cc ccc cccc", "6 6 Fri Friday"),
        ("a aa aaa aaaa aaaaa", "PM PM PM PM p"),
        ("S SS SSSS", "5 50 5000"),
        ("x xx xxx X XXX ZZZZ O OOOO", "+00 +0000 +00:00 Z Z GMT+00:00 GMT+0 GMT+00:00"),
    ] {
        f.setDateFormat(Some(&ns(pattern)));
        assert_eq!(f.stringFromDate(&at(MOMENT)).to_string(), text, "{pattern:?}");
    }
    f.setDateFormat(Some(&ns("y yyyy G u")));
    assert_eq!(f.stringFromDate(&at(-63_200_000_000.0)).to_string(), "3 0003 BC -2");

    f.setTimeZone(Some(&zone("America/New_York")));
    f.setDateFormat(Some(&ns("z zzzz Z ZZZZZ O v vvvv VV VVV VVVV X")));
    assert_eq!(
        f.stringFromDate(&at(MOMENT)).to_string(),
        "EDT Eastern Daylight Time -0400 -04:00 GMT-4 ET Eastern Time America/New_York New York New York Time -04"
    );
    f.setTimeZone(Some(&zone("Europe/Paris")));
    f.setDateFormat(Some(&ns("z zzzz")));
    assert_eq!(f.stringFromDate(&at(MOMENT)).to_string(), "GMT+2 Central European Summer Time");
}

#[test]
fn parsing() {
    let f = formatter("en_US_POSIX", &NSTimeZone::timeZoneForSecondsFromGMT(0));
    let parse = |pattern: &str, text: &str| {
        f.setDateFormat(Some(&ns(pattern)));
        f.dateFromString(&ns(text)).map(|d| d.timeIntervalSinceReferenceDate())
    };
    let day = |y: i64, m: i64, d: i64| {
        // Days from 2001-01-01 by the proleptic Gregorian rules.
        let days = |y: i64, m: i64, d: i64| {
            let y = if m <= 2 { y - 1 } else { y };
            let era = y.div_euclid(400);
            let yoe = y - era * 400;
            let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
            era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
        };
        ((days(y, m, d) - days(2001, 1, 1)) * 86_400) as f64
    };
    assert_eq!(parse("yyyy-MM-dd", "2024-02-30"), None);
    assert_eq!(parse("yyyy-MM-dd", "2024-2-3"), Some(day(2024, 2, 3)));
    assert_eq!(parse("yyyy-MM-dd", " 2024-02-03"), Some(day(2024, 2, 3)));
    assert_eq!(parse("yyyy-MM-dd", "2024-02-03 "), Some(day(2024, 2, 3)));
    assert_eq!(parse("yyyy-MM-dd", "2024-02-03x"), None);
    assert_eq!(parse("yyyy-MM-dd", "24-02-03"), Some(-62_385_465_600.0), "the Julian calendar before 1582");
    assert_eq!(parse("yyyy-MM-dd", "2024-13-01"), None);
    assert_eq!(parse("yy", "25"), Some(day(2025, 1, 1)));
    assert_eq!(parse("yy", "49"), Some(day(2049, 1, 1)));
    assert_eq!(parse("yy", "50"), Some(day(1950, 1, 1)));
    assert_eq!(parse("yy", "99"), Some(day(1999, 1, 1)));
    assert_eq!(parse("MMM d", "sep 19"), Some(day(2000, 9, 19)));
    assert_eq!(parse("MMMM d yyyy", "September 19 2025"), Some(day(2025, 9, 19)));
    assert_eq!(parse("h:mm a", "6:40 pm"), Some(day(2000, 1, 1) + 67_200.0));
    assert_eq!(parse("h a", "12 am"), Some(day(2000, 1, 1)));
    assert_eq!(parse("h a", "12 pm"), Some(day(2000, 1, 1) + 43_200.0));
    assert_eq!(parse("HH:mm z", "10:00 EST"), Some(day(2000, 1, 1) + 54_000.0));
    assert_eq!(parse("HH:mm z", "10:00 GMT+5:30"), Some(day(2000, 1, 1) + 16_200.0));
    assert_eq!(parse("HH:mm Z", "10:00 -0500"), Some(day(2000, 1, 1) + 54_000.0));
    assert_eq!(parse("HH:mm ZZZZZ", "10:00 -05:00"), Some(day(2000, 1, 1) + 54_000.0));
    assert_eq!(parse("HH:mm zzzz", "10:00 Eastern Standard Time"), Some(day(2000, 1, 1) + 54_000.0));
    assert_eq!(parse("EEE MMM d", "Mon Sep 19"), Some(day(2000, 9, 19)), "the weekday is read but not used");
    assert_eq!(parse("yyyyMMdd", "20250919"), Some(day(2025, 9, 19)));
    assert_eq!(parse("HHmmss", "184000"), Some(day(2000, 1, 1) + 67_200.0));
    assert_eq!(parse("d/M/y", "31/12/2025"), Some(day(2025, 12, 31)));
    assert_eq!(parse("yyyy", "-5"), None);
    assert_eq!(parse("H:mm", "24:00"), None);
    assert_eq!(parse("H:mm", "7:60"), None);
    let fraction = parse("yyyy-MM-dd HH:mm:ss.SSS", "2025-09-19 18:40:00.123").unwrap();
    assert!((fraction - 780_000_000.123).abs() < 1e-3);

    f.setLenient(true);
    assert!(f.isLenient());
    assert_eq!(parse("yyyy-MM-dd", "2024-02-30"), Some(day(2024, 3, 1)));
    assert_eq!(parse("yyyy-MM-dd", "2024-13-01"), Some(day(2025, 1, 1)));

    // A named zone reads local times in that zone.
    let ny = formatter("en_US_POSIX", &zone("America/New_York"));
    ny.setDateFormat(Some(&ns("yyyy-MM-dd HH:mm")));
    assert_eq!(
        ny.dateFromString(&ns("2025-09-19 14:40")).map(|d| d.timeIntervalSinceReferenceDate()),
        Some(780_000_000.0)
    );
    assert_eq!(
        ny.dateFromString(&ns("2025-01-15 12:00")).map(|d| d.timeIntervalSinceReferenceDate()),
        Some(day(2025, 1, 15) + 61_200.0)
    );
}

#[test]
fn templates_and_relative_dates() {
    let us = NSLocale::localeWithLocaleIdentifier(&ns("en_US"));
    for (template, pattern) in [
        ("yMMMd", "MMM d, y"),
        ("jmm", "h:mm\u{202f}a"),
        ("MMMMdyyyy", "MMMM d, yyyy"),
        ("Hm", "HH:mm"),
        ("yMd", "M/d/y"),
        ("EEEEdMMM", "EEEE, MMM d"),
        ("yMMMMEEEEd", "EEEE, MMMM d, y"),
        ("jmmss", "h:mm:ss\u{202f}a"),
        ("Hmmss", "HH:mm:ss"),
        ("yM", "M/y"),
        ("yMMM", "MMM y"),
        ("MMMd", "MMM d"),
        ("Md", "M/d"),
        ("Ed", "EEE d"),
        ("yMEd", "EEE, M/d/y"),
        ("MMMMd", "MMMM d"),
        ("yQQQ", "QQQ y"),
        ("hmmz", "h:mm\u{202f}a z"),
        ("jmmzzzz", "h:mm\u{202f}a zzzz"),
        ("d", "d"),
        ("MMMM", "LLLL"),
        ("EEEE", "cccc"),
        ("y", "y"),
        ("yyMMdd", "MM/dd/yy"),
    ] {
        let made = NSDateFormatter::dateFormatFromTemplate_options_locale(&ns(template), 0, Some(&us));
        assert_eq!(made.map(|p| p.to_string()).as_deref(), Some(pattern), "{template}");
    }
    let f = formatter("en_US", &NSTimeZone::timeZoneForSecondsFromGMT(0));
    f.setLocalizedDateFormatFromTemplate(&ns("yMMMd"));
    assert_eq!(f.dateFormat().to_string(), "MMM d, y");
    assert_eq!(f.stringFromDate(&at(MOMENT)).to_string(), "Sep 19, 2025");

    let r = formatter("en_US", &NSTimeZone::timeZoneForSecondsFromGMT(0));
    r.setDateStyle(NSDateFormatterStyle::MediumStyle);
    r.setDoesRelativeDateFormatting(true);
    assert_eq!(r.stringFromDate(&NSDate::date()).to_string(), "Today");
    assert_eq!(r.stringFromDate(&NSDate::dateWithTimeIntervalSinceNow(86_400.0)).to_string(), "Tomorrow");
    assert_eq!(r.stringFromDate(&NSDate::dateWithTimeIntervalSinceNow(-86_400.0)).to_string(), "Yesterday");
    assert_eq!(r.stringFromDate(&at(MOMENT)).to_string(), "Sep 19, 2025", "far dates keep their style");
    r.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    assert!(r.stringFromDate(&NSDate::date()).to_string().starts_with("Today at "));
}

#[test]
#[allow(deprecated)] // countryCode, which regionCode replaces
fn locales() {
    let l = NSLocale::localeWithLocaleIdentifier(&ns("en_US"));
    assert_eq!(l.localeIdentifier().to_string(), "en_US");
    assert_eq!(l.languageCode().to_string(), "en");
    assert_eq!(l.countryCode().map(|c| c.to_string()).as_deref(), Some("US"));
    assert!(l.isEqual(Some(&NSLocale::localeWithLocaleIdentifier(&ns("en_US")))));
    assert!(!l.isEqual(Some(&NSLocale::localeWithLocaleIdentifier(&ns("en_GB")))));
    assert_eq!(
        NSLocale::localeWithLocaleIdentifier(&ns("fr_FR.UTF-8")).localeIdentifier().to_string(),
        "fr_FR.UTF-8",
        "kept as given"
    );
    assert_eq!(NSLocale::localeWithLocaleIdentifier(&ns("en-GB")).localeIdentifier().to_string(), "en-GB");
    assert_eq!(NSLocale::systemLocale().localeIdentifier().to_string(), "");
    unsafe {
        use objc2_foundation::{
            NSCurrentLocaleDidChangeNotification, NSLocaleCountryCode, NSLocaleDecimalSeparator, NSLocaleIdentifier,
            NSLocaleLanguageCode, NSLocaleUsesMetricSystem,
        };
        assert_eq!(NSLocaleIdentifier.to_string(), "kCFLocaleIdentifierKey");
        assert_eq!(NSLocaleLanguageCode.to_string(), "kCFLocaleLanguageCodeKey");
        assert_eq!(NSLocaleCountryCode.to_string(), "kCFLocaleCountryCodeKey");
        assert_eq!(NSLocaleDecimalSeparator.to_string(), "kCFLocaleDecimalSeparatorKey");
        assert_eq!(NSLocaleUsesMetricSystem.to_string(), "kCFLocaleUsesMetricSystemKey");
        assert_eq!(NSCurrentLocaleDidChangeNotification.to_string(), "kCFLocaleCurrentLocaleDidChangeNotification");
        let language = l.objectForKey(NSLocaleLanguageCode).map(|v| v.downcast::<NSString>().unwrap().to_string());
        assert_eq!(language.as_deref(), Some("en"));
    }
    assert_eq!(l.decimalSeparator().to_string(), ".");
    assert!(!l.usesMetricSystem());
}

#[test]
fn time_zones() {
    let ny = zone("America/New_York");
    assert_eq!(ny.name().to_string(), "America/New_York");
    assert_eq!(ny.secondsFromGMTForDate(&at(MOMENT)), -14_400);
    assert_eq!(ny.abbreviationForDate(&at(MOMENT)).map(|a| a.to_string()).as_deref(), Some("EDT"));
    assert!(ny.isDaylightSavingTimeForDate(&at(MOMENT)));
    assert_eq!(ny.daylightSavingTimeOffsetForDate(&at(MOMENT)), 3600.0);
    let january = at(757_000_000.0);
    assert_eq!(ny.secondsFromGMTForDate(&january), -18_000);
    assert_eq!(ny.abbreviationForDate(&january).map(|a| a.to_string()).as_deref(), Some("EST"));
    assert!(!ny.isDaylightSavingTimeForDate(&january));
    assert_eq!(
        ny.nextDaylightSavingTimeTransitionAfterDate(&january).map(|d| d.timeIntervalSinceReferenceDate()),
        Some(763_196_400.0)
    );
    assert!(ny.isEqual(Some(&zone("America/New_York"))));
    assert!(NSTimeZone::timeZoneWithName(&ns("Nowhere/Nope")).is_none());

    let fixed = NSTimeZone::timeZoneForSecondsFromGMT(19_800);
    assert_eq!(fixed.name().to_string(), "GMT+0530");
    assert_eq!(fixed.abbreviation().map(|a| a.to_string()).as_deref(), Some("GMT+5:30"));
    assert_eq!(fixed.secondsFromGMT(), 19_800);
    let gmt = NSTimeZone::timeZoneForSecondsFromGMT(0);
    assert_eq!(gmt.name().to_string(), "GMT");
    assert_eq!(gmt.abbreviation().map(|a| a.to_string()).as_deref(), Some("GMT"));
    for name in ["GMT", "UTC"] {
        assert_eq!(zone(name).name().to_string(), "GMT");
    }
    assert_eq!(
        NSTimeZone::timeZoneWithAbbreviation(&ns("EST")).map(|z| z.name().to_string()).as_deref(),
        Some("America/New_York")
    );
    let description: Retained<NSString> = unsafe { msg_send![&*fixed, description] };
    assert_eq!(description.to_string(), "GMT+0530 (GMT+5:30) offset 19800");
}

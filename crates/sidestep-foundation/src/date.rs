//! `NSDate`: an immutable moment, stored as seconds since the reference
//! date, 2001-01-01 00:00:00 UTC.
//!
//! Dates compare exactly (no tolerance), and `-description` prints UTC as
//! `2001-01-01 00:00:00 +0000`, truncating fractions of a second. Like
//! macOS it prints dates before 15 October 1582 in the Julian calendar,
//! which is why `+distantPast` reads as 0001-01-01.

use std::time::{SystemTime, UNIX_EPOCH};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSComparisonResult, NSDate, NSString, NSTimeInterval, NSUInteger, NSZone};

/// Seconds from 1970 to 2001.
pub(crate) const UNIX_TO_REFERENCE: f64 = 978_307_200.0;

/// `+distantFuture` and `+distantPast`, in seconds since 2001.
const DISTANT_FUTURE: f64 = 63_113_904_000.0;
const DISTANT_PAST: f64 = -63_114_076_800.0;

/// The current time in seconds since 2001 (`CFAbsoluteTimeGetCurrent`).
pub(crate) fn now() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_secs_f64() - UNIX_TO_REFERENCE,
        Err(before) => -before.duration().as_secs_f64() - UNIX_TO_REFERENCE,
    }
}

sidestep_runtime::static_class!(pub(crate) NSDATE, NSDATE_META = "NSDate", || {
    let _ = NSDateImpl::class();
    crate::perform::install();
});

pub(crate) struct DateIvars {
    time: f64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSDate"]
    #[ivars = DateIvars]
    pub(crate) struct NSDateImpl;

    impl NSDateImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            Self::init_at(this, now())
        }

        #[unsafe(method_id(initWithTimeIntervalSinceReferenceDate:))]
        fn init_with_reference(this: Allocated<Self>, time: NSTimeInterval) -> Retained<Self> {
            Self::init_at(this, time)
        }

        #[unsafe(method_id(initWithTimeIntervalSinceNow:))]
        fn init_with_now(this: Allocated<Self>, seconds: NSTimeInterval) -> Retained<Self> {
            Self::init_at(this, now() + seconds)
        }

        #[unsafe(method_id(initWithTimeIntervalSince1970:))]
        fn init_with_1970(this: Allocated<Self>, seconds: NSTimeInterval) -> Retained<Self> {
            Self::init_at(this, seconds - UNIX_TO_REFERENCE)
        }

        #[unsafe(method_id(initWithTimeInterval:sinceDate:))]
        fn init_with_since(this: Allocated<Self>, seconds: NSTimeInterval, date: &NSDate) -> Retained<Self> {
            Self::init_at(this, time_of(date) + seconds)
        }

        #[unsafe(method_id(date))]
        fn date() -> Retained<Self> {
            Self::at(now())
        }

        #[unsafe(method_id(now))]
        fn now_date() -> Retained<Self> {
            Self::at(now())
        }

        #[unsafe(method_id(dateWithTimeIntervalSinceNow:))]
        fn with_since_now(seconds: NSTimeInterval) -> Retained<Self> {
            Self::at(now() + seconds)
        }

        #[unsafe(method_id(dateWithTimeIntervalSinceReferenceDate:))]
        fn with_reference(time: NSTimeInterval) -> Retained<Self> {
            Self::at(time)
        }

        #[unsafe(method_id(dateWithTimeIntervalSince1970:))]
        fn with_1970(seconds: NSTimeInterval) -> Retained<Self> {
            Self::at(seconds - UNIX_TO_REFERENCE)
        }

        #[unsafe(method_id(dateWithTimeInterval:sinceDate:))]
        fn with_since(seconds: NSTimeInterval, date: &NSDate) -> Retained<Self> {
            Self::at(time_of(date) + seconds)
        }

        #[unsafe(method_id(distantFuture))]
        fn distant_future() -> Retained<Self> {
            Self::at(DISTANT_FUTURE)
        }

        #[unsafe(method_id(distantPast))]
        fn distant_past() -> Retained<Self> {
            Self::at(DISTANT_PAST)
        }

        #[unsafe(method(timeIntervalSinceReferenceDate))]
        fn class_time_interval_since_reference_date() -> NSTimeInterval {
            now()
        }

        #[unsafe(method(timeIntervalSinceReferenceDate))]
        fn time_interval_since_reference_date(&self) -> NSTimeInterval {
            self.ivars().time
        }

        #[unsafe(method(timeIntervalSinceDate:))]
        fn time_interval_since_date(&self, other: &NSDate) -> NSTimeInterval {
            self.ivars().time - time_of(other)
        }

        #[unsafe(method(timeIntervalSinceNow))]
        fn time_interval_since_now(&self) -> NSTimeInterval {
            self.ivars().time - now()
        }

        #[unsafe(method(timeIntervalSince1970))]
        fn time_interval_since_1970(&self) -> NSTimeInterval {
            self.ivars().time + UNIX_TO_REFERENCE
        }

        #[unsafe(method_id(dateByAddingTimeInterval:))]
        fn date_by_adding(&self, seconds: NSTimeInterval) -> Retained<Self> {
            Self::at(self.ivars().time + seconds)
        }

        #[unsafe(method_id(addTimeInterval:))]
        fn add_time_interval(&self, seconds: NSTimeInterval) -> Retained<AnyObject> {
            Self::at(self.ivars().time + seconds).into_super().into_super()
        }

        #[unsafe(method_id(earlierDate:))]
        fn earlier_date(&self, other: &NSDate) -> Retained<NSDate> {
            // The receiver wins ties, as on macOS.
            if time_of(other) < self.ivars().time { other.retain() } else { self.as_ns() }
        }

        #[unsafe(method_id(laterDate:))]
        fn later_date(&self, other: &NSDate) -> Retained<NSDate> {
            if time_of(other) > self.ivars().time { other.retain() } else { self.as_ns() }
        }

        #[unsafe(method(compare:))]
        fn compare(&self, other: &NSDate) -> NSComparisonResult {
            compare(self.ivars().time, time_of(other))
        }

        #[unsafe(method(isEqualToDate:))]
        fn is_equal_to_date(&self, other: &NSDate) -> bool {
            self.ivars().time == time_of(other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            match other.and_then(|o| o.downcast_ref::<NSDate>()) {
                Some(other) => self.ivars().time == time_of(other),
                None => false,
            }
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            hash(self.ivars().time)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&describe(self.ivars().time))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSDateImpl {}
);

impl NSDateImpl {
    fn init_at(this: Allocated<Self>, time: f64) -> Retained<Self> {
        let this = this.set_ivars(DateIvars { time });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Only called from the class's own methods, so the class is loaded.
    fn at(time: f64) -> Retained<Self> {
        Self::init_at(Self::alloc(), time)
    }

    fn as_ns(&self) -> Retained<NSDate> {
        // SAFETY: NSDateImpl is the class NSDate names.
        unsafe { Retained::cast_unchecked(self.retain()) }
    }
}

/// A date's seconds since 2001.
pub(crate) fn time_of(date: &NSDate) -> f64 {
    date.timeIntervalSinceReferenceDate()
}

fn compare(a: f64, b: f64) -> NSComparisonResult {
    if a < b {
        NSComparisonResult::Ascending
    } else if a > b {
        NSComparisonResult::Descending
    } else {
        NSComparisonResult::Same
    }
}

/// Equal dates hash equally. Whole seconds hash as macOS does (Knuth's
/// multiplicative constant); others mix their bits.
fn hash(time: f64) -> NSUInteger {
    if time.fract() == 0.0 && time.abs() < 9.0e15 {
        (time as i64 as NSUInteger).wrapping_mul(2_654_435_761)
    } else {
        let bits = time.to_bits().wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (bits ^ (bits >> 29)) as NSUInteger
    }
}

/// `2001-01-01 00:00:00 +0000`, or an empty string for NaN.
pub(crate) fn describe(time: f64) -> String {
    if !time.is_finite() {
        return String::new();
    }
    let unix = (time + UNIX_TO_REFERENCE).floor() as i64;
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} +0000", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Days since 1970-01-01 at which the Gregorian calendar starts
/// (1582-10-15); earlier days are named in the Julian calendar.
const GREGORIAN_START: i64 = -141_427;

/// The calendar date of a day number, in the calendar macOS prints it in.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    if days >= GREGORIAN_START { gregorian_from_days(days) } else { julian_from_days(days) }
}

/// Howard Hinnant's `civil_from_days` for the proleptic Gregorian calendar.
fn gregorian_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The same for the Julian calendar, whose 4-year cycles have 1461 days.
fn julian_from_days(days: i64) -> (i64, u32, u32) {
    // 1970-01-01 Gregorian is 1969-12-19 Julian; count from 0000-03-01
    // Julian, day -719_470 relative to the Unix epoch.
    let z = days + 719_470;
    let era = z.div_euclid(1461);
    let doe = z.rem_euclid(1461);
    let yoe = (doe - doe / 1460) / 365;
    let doy = doe - 365 * yoe;
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 4 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendars() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(GREGORIAN_START), (1582, 10, 15));
        assert_eq!(civil_from_days(GREGORIAN_START - 1), (1582, 10, 4));
        assert_eq!(describe(DISTANT_PAST), "0001-01-01 00:00:00 +0000");
        assert_eq!(describe(DISTANT_FUTURE), "4001-01-01 00:00:00 +0000");
        assert_eq!(describe(0.0), "2001-01-01 00:00:00 +0000");
        assert_eq!(describe(-1.0e9), "1969-04-24 22:13:20 +0000");
        assert_eq!(describe(123_456_789.987), "2004-11-29 21:33:09 +0000");
    }
}

//! `NSTimeZone`, over jiff's IANA time zones.
//!
//! A time zone is either a named zone from the system's tz database
//! (`/usr/share/zoneinfo`, through jiff) or a fixed offset from GMT. The
//! system zone comes from `$TZ`, then `/etc/localtime`, as the C library
//! has it; the default zone is the system zone until set.
//!
//! Abbreviations are the database's, except that numeric ones and fixed
//! offsets read as macOS writes them (`GMT+5:30`). Long names ("Eastern
//! Daylight Time"), used by date formatting, are known for GMT and the US
//! zones; other zones use the GMT offset, as CLDR does without a name.

use std::sync::{Mutex, OnceLock};

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSDate, NSInteger, NSString, NSTimeZone, NSUInteger, NSZone};

sidestep_runtime::static_class!(pub(crate) NSTIMEZONE, NSTIMEZONE_META = "NSTimeZone", || {
    let _ = NSTimeZoneImpl::class();
    crate::perform::install();
});

crate::runloop::modes::exported_strings! {
    NSSystemTimeZoneDidChangeNotification, SYSTEM_TIME_ZONE_DID_CHANGE = "NSSystemTimeZoneDidChangeNotification";
}

/// A zone: its Foundation name and jiff's zone.
#[derive(Clone, Debug)]
pub(crate) struct Zone {
    pub(crate) name: String,
    pub(crate) tz: TimeZone,
    /// A fixed offset, for zones made from one.
    pub(crate) fixed: Option<i32>,
}

/// What a zone says about one moment.
pub(crate) struct Moment {
    pub(crate) offset: i32,
    pub(crate) dst: bool,
    pub(crate) abbreviation: String,
}

/// A timestamp from seconds since the reference date.
pub(crate) fn timestamp(seconds: f64) -> Timestamp {
    let unix = seconds + crate::date::UNIX_TO_REFERENCE;
    let whole = unix.floor();
    let nanos = ((unix - whole) * 1e9) as i32;
    Timestamp::new(whole as i64, nanos.clamp(0, 999_999_999)).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// `GMT+5:30`, `GMT-4`, `GMT`.
pub(crate) fn short_gmt(offset: i32) -> String {
    if offset == 0 {
        return "GMT".into();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let (h, m) = (offset.unsigned_abs() / 3600, offset.unsigned_abs() / 60 % 60);
    if m == 0 { format!("GMT{sign}{h}") } else { format!("GMT{sign}{h}:{m:02}") }
}

/// `GMT+05:30`, `GMT+00:00`.
pub(crate) fn long_gmt(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let (h, m) = (offset.unsigned_abs() / 3600, offset.unsigned_abs() / 60 % 60);
    format!("GMT{sign}{h:02}:{m:02}")
}

impl Zone {
    pub(crate) fn named(name: &str) -> Option<Zone> {
        if matches!(name, "GMT" | "UTC" | "UT" | "Z" | "Etc/GMT" | "Etc/UTC") {
            return Some(Zone::fixed(0));
        }
        let tz = TimeZone::get(name).ok()?;
        Some(Zone { name: name.to_string(), tz, fixed: None })
    }

    pub(crate) fn fixed(seconds: i32) -> Zone {
        let offset = Offset::from_seconds(seconds).unwrap_or(Offset::UTC);
        let name = if seconds == 0 {
            "GMT".to_string()
        } else {
            let sign = if seconds < 0 { '-' } else { '+' };
            let (h, m) = (seconds.unsigned_abs() / 3600, seconds.unsigned_abs() / 60 % 60);
            format!("GMT{sign}{h:02}{m:02}")
        };
        Zone { name, tz: TimeZone::fixed(offset), fixed: Some(seconds) }
    }

    /// The system's zone: `$TZ`, then `/etc/localtime`.
    pub(crate) fn system() -> Zone {
        if let Ok(name) = std::env::var("TZ") {
            let name = name.trim_start_matches(':');
            if let Some(zone) = Zone::named(name) {
                return zone;
            }
        }
        let tz = TimeZone::try_system().unwrap_or(TimeZone::UTC);
        let name = tz.iana_name().map(str::to_string).or_else(|| {
            std::fs::read_link("/etc/localtime")
                .ok()
                .and_then(|p| p.to_str().and_then(|p| p.split("zoneinfo/").nth(1)).map(str::to_string))
        });
        match name {
            Some(name) if name == "UTC" || name == "Etc/UTC" => Zone::fixed(0),
            Some(name) => Zone { name, tz, fixed: None },
            None => Zone::fixed(0),
        }
    }

    pub(crate) fn at(&self, seconds: f64) -> Moment {
        if let Some(offset) = self.fixed {
            return Moment { offset, dst: false, abbreviation: short_gmt(offset) };
        }
        let info = self.tz.to_offset_info(timestamp(seconds));
        let offset = info.offset().seconds();
        let abbreviation = info.abbreviation();
        let abbreviation = if abbreviation.starts_with(['+', '-']) || abbreviation.is_empty() {
            short_gmt(offset)
        } else {
            abbreviation.to_string()
        };
        Moment { offset, dst: info.dst().is_dst(), abbreviation }
    }

    /// The next change of offset after a moment, in seconds since the
    /// reference date.
    pub(crate) fn next_transition(&self, seconds: f64) -> Option<f64> {
        if self.fixed.is_some() {
            return None;
        }
        let next = self.tz.following(timestamp(seconds)).next()?;
        Some(next.timestamp().as_second() as f64 - crate::date::UNIX_TO_REFERENCE)
    }

    /// The zone's English names, if CLDR's English has them.
    pub(crate) fn metazone(&self) -> Option<&'static Metazone> {
        let name = self.name.as_str();
        METAZONES.iter().find(|m| m.zones.contains(&name))
    }

    /// `z`: the US abbreviations, else the short GMT offset.
    pub(crate) fn short_specific(&self, seconds: f64) -> String {
        let moment = self.at(seconds);
        match self.metazone().and_then(|m| m.short) {
            Some((standard, daylight, _)) => (if moment.dst { daylight } else { standard }).to_string(),
            None if self.name == "GMT" => "GMT".into(),
            None => short_gmt(moment.offset),
        }
    }

    /// `zzzz`: "Eastern Daylight Time", or the long GMT offset.
    pub(crate) fn long_name(&self, seconds: f64) -> String {
        let moment = self.at(seconds);
        match self.metazone() {
            Some(m) => (if moment.dst { m.daylight } else { m.standard }).to_string(),
            None if self.name == "GMT" => "Greenwich Mean Time".into(),
            None => long_gmt(moment.offset),
        }
    }

    /// `v`: "ET", else the location's name.
    pub(crate) fn generic_short(&self, seconds: f64) -> String {
        match self.metazone() {
            Some(Metazone { short: Some((_, _, generic)), .. }) => (*generic).to_string(),
            _ if self.name == "GMT" => "GMT".into(),
            _ if self.fixed.is_some() => short_gmt(self.at(seconds).offset),
            _ => self.generic_location(seconds),
        }
    }

    /// `vvvv`: "Eastern Time", else the location's name.
    pub(crate) fn generic_long(&self, seconds: f64) -> String {
        match self.metazone() {
            Some(m) => m.generic.to_string(),
            None if self.name == "GMT" => "Greenwich Mean Time".into(),
            None if self.fixed.is_some() => long_gmt(self.at(seconds).offset),
            None => self.generic_location(seconds),
        }
    }

    /// `VVV`: the city the zone is named for.
    pub(crate) fn exemplar_city(&self) -> String {
        if self.fixed.is_some() || !self.name.contains('/') {
            return "Unknown Location".into();
        }
        self.name.rsplit('/').next().unwrap_or("").replace('_', " ")
    }

    /// `VVVV`: "New York Time", or the long GMT offset.
    pub(crate) fn generic_location(&self, seconds: f64) -> String {
        if self.fixed.is_some() || !self.name.contains('/') {
            return long_gmt(self.at(seconds).offset);
        }
        format!("{} Time", self.exemplar_city())
    }
}

/// A CLDR metazone's English names.
pub(crate) struct Metazone {
    zones: &'static [&'static str],
    standard: &'static str,
    daylight: &'static str,
    generic: &'static str,
    /// Abbreviations English uses: standard, daylight, generic.
    short: Option<(&'static str, &'static str, &'static str)>,
}

static METAZONES: &[Metazone] = &[
    Metazone {
        zones: &[
            "America/New_York",
            "America/Detroit",
            "America/Toronto",
            "America/Indiana/Indianapolis",
            "US/Eastern",
            "EST5EDT",
        ],
        standard: "Eastern Standard Time",
        daylight: "Eastern Daylight Time",
        generic: "Eastern Time",
        short: Some(("EST", "EDT", "ET")),
    },
    Metazone {
        zones: &["America/Chicago", "America/Winnipeg", "US/Central", "CST6CDT"],
        standard: "Central Standard Time",
        daylight: "Central Daylight Time",
        generic: "Central Time",
        short: Some(("CST", "CDT", "CT")),
    },
    Metazone {
        zones: &["America/Denver", "America/Phoenix", "America/Boise", "America/Edmonton", "US/Mountain", "MST7MDT"],
        standard: "Mountain Standard Time",
        daylight: "Mountain Daylight Time",
        generic: "Mountain Time",
        short: Some(("MST", "MDT", "MT")),
    },
    Metazone {
        zones: &["America/Los_Angeles", "America/Vancouver", "US/Pacific", "PST8PDT"],
        standard: "Pacific Standard Time",
        daylight: "Pacific Daylight Time",
        generic: "Pacific Time",
        short: Some(("PST", "PDT", "PT")),
    },
    Metazone {
        zones: &["America/Anchorage", "US/Alaska"],
        standard: "Alaska Standard Time",
        daylight: "Alaska Daylight Time",
        generic: "Alaska Time",
        short: Some(("AKST", "AKDT", "AKT")),
    },
    Metazone {
        zones: &["Pacific/Honolulu", "US/Hawaii"],
        standard: "Hawaii-Aleutian Standard Time",
        daylight: "Hawaii-Aleutian Daylight Time",
        generic: "Hawaii-Aleutian Time",
        short: Some(("HST", "HDT", "HST")),
    },
    Metazone {
        zones: &[
            "Europe/Paris",
            "Europe/Berlin",
            "Europe/Madrid",
            "Europe/Rome",
            "Europe/Amsterdam",
            "Europe/Brussels",
            "Europe/Vienna",
            "Europe/Stockholm",
            "Europe/Oslo",
            "Europe/Copenhagen",
            "Europe/Warsaw",
            "Europe/Prague",
            "Europe/Zurich",
            "Europe/Budapest",
            "Europe/Belgrade",
        ],
        standard: "Central European Standard Time",
        daylight: "Central European Summer Time",
        generic: "Central European Time",
        short: None,
    },
    Metazone {
        zones: &[
            "Europe/Athens",
            "Europe/Helsinki",
            "Europe/Kyiv",
            "Europe/Kiev",
            "Europe/Bucharest",
            "Europe/Sofia",
            "Europe/Riga",
            "Europe/Vilnius",
            "Europe/Tallinn",
        ],
        standard: "Eastern European Standard Time",
        daylight: "Eastern European Summer Time",
        generic: "Eastern European Time",
        short: None,
    },
    Metazone {
        zones: &["Europe/Lisbon"],
        standard: "Western European Standard Time",
        daylight: "Western European Summer Time",
        generic: "Western European Time",
        short: None,
    },
    Metazone {
        zones: &["Europe/London"],
        standard: "Greenwich Mean Time",
        daylight: "British Summer Time",
        generic: "United Kingdom Time",
        short: None,
    },
    Metazone {
        zones: &["Europe/Moscow"],
        standard: "Moscow Standard Time",
        daylight: "Moscow Summer Time",
        generic: "Moscow Time",
        short: None,
    },
    Metazone {
        zones: &["Asia/Kolkata", "Asia/Calcutta"],
        standard: "India Standard Time",
        daylight: "India Standard Time",
        generic: "India Standard Time",
        short: None,
    },
    Metazone {
        zones: &["Asia/Tokyo"],
        standard: "Japan Standard Time",
        daylight: "Japan Daylight Time",
        generic: "Japan Time",
        short: None,
    },
    Metazone {
        zones: &["Asia/Shanghai"],
        standard: "China Standard Time",
        daylight: "China Daylight Time",
        generic: "China Time",
        short: None,
    },
    Metazone {
        zones: &["Asia/Seoul"],
        standard: "Korean Standard Time",
        daylight: "Korean Daylight Time",
        generic: "Korean Time",
        short: None,
    },
    Metazone {
        zones: &["Australia/Sydney", "Australia/Melbourne", "Australia/Hobart", "Australia/Brisbane"],
        standard: "Australian Eastern Standard Time",
        daylight: "Australian Eastern Daylight Time",
        generic: "Eastern Australia Time",
        short: None,
    },
];

pub(crate) struct ZoneIvars {
    zone: Zone,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTimeZone"]
    #[ivars = ZoneIvars]
    pub(crate) struct NSTimeZoneImpl;

    impl NSTimeZoneImpl {
        #[unsafe(method_id(timeZoneWithName:))]
        fn with_name(name: &NSString) -> Option<Retained<Self>> {
            Zone::named(&name.to_string()).map(make)
        }

        #[unsafe(method_id(timeZoneWithName:data:))]
        fn with_name_data(name: &NSString, _data: Option<&AnyObject>) -> Option<Retained<Self>> {
            Zone::named(&name.to_string()).map(make)
        }

        #[unsafe(method_id(initWithName:))]
        fn init_with_name(this: Allocated<Self>, name: &NSString) -> Option<Retained<Self>> {
            init_zone(this, Zone::named(&name.to_string()))
        }

        #[unsafe(method_id(initWithName:data:))]
        fn init_with_name_data(this: Allocated<Self>, name: &NSString, _data: Option<&AnyObject>) -> Option<Retained<Self>> {
            init_zone(this, Zone::named(&name.to_string()))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            init_zone(this, Some(default_zone()))
        }

        #[unsafe(method_id(timeZoneForSecondsFromGMT:))]
        fn for_seconds_from_gmt(seconds: NSInteger) -> Option<Retained<Self>> {
            // macOS allows up to 18 hours either way.
            (seconds.abs() <= 18 * 3600).then(|| make(Zone::fixed(seconds as i32)))
        }

        #[unsafe(method_id(timeZoneWithAbbreviation:))]
        fn with_abbreviation(abbreviation: &NSString) -> Option<Retained<Self>> {
            zone_for_abbreviation(&abbreviation.to_string()).and_then(Zone::named).map(make)
        }

        #[unsafe(method_id(systemTimeZone))]
        fn system_time_zone() -> Retained<Self> {
            make(system_zone())
        }

        #[unsafe(method(resetSystemTimeZone))]
        fn reset_system_time_zone() {
            *crate::thread::lock(system_cache()) = None;
        }

        #[unsafe(method_id(defaultTimeZone))]
        fn default_time_zone() -> Retained<Self> {
            make(default_zone())
        }

        #[unsafe(method(setDefaultTimeZone:))]
        fn set_default_time_zone(zone: &NSTimeZone) {
            *crate::thread::lock(default_cache()) = Some(zone_of(zone).clone());
        }

        #[unsafe(method_id(localTimeZone))]
        fn local_time_zone() -> Retained<Self> {
            make(default_zone())
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(knownTimeZoneNames))]
        fn known_time_zone_names() -> Retained<AnyObject> {
            let names: Vec<Retained<NSString>> = known_names().iter().map(|n| NSString::from_str(n)).collect();
            objc2_foundation::NSArray::from_retained_slice(&names).into()
        }

        #[unsafe(method_id(timeZoneDataVersion))]
        fn time_zone_data_version() -> Retained<NSString> {
            NSString::from_str(std::fs::read_to_string("/usr/share/zoneinfo/+VERSION").unwrap_or_default().trim())
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().zone.name)
        }

        #[unsafe(method(secondsFromGMT))]
        fn seconds_from_gmt(&self) -> NSInteger {
            self.ivars().zone.at(crate::date::now()).offset as NSInteger
        }

        #[unsafe(method(secondsFromGMTForDate:))]
        fn seconds_from_gmt_for_date(&self, date: &NSDate) -> NSInteger {
            self.ivars().zone.at(crate::date::time_of(date)).offset as NSInteger
        }

        #[unsafe(method_id(abbreviation))]
        fn abbreviation(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().zone.at(crate::date::now()).abbreviation))
        }

        #[unsafe(method_id(abbreviationForDate:))]
        fn abbreviation_for_date(&self, date: &NSDate) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().zone.at(crate::date::time_of(date)).abbreviation))
        }

        #[unsafe(method(isDaylightSavingTime))]
        fn is_daylight_saving_time(&self) -> bool {
            self.ivars().zone.at(crate::date::now()).dst
        }

        #[unsafe(method(isDaylightSavingTimeForDate:))]
        fn is_daylight_saving_time_for_date(&self, date: &NSDate) -> bool {
            self.ivars().zone.at(crate::date::time_of(date)).dst
        }

        #[unsafe(method(daylightSavingTimeOffset))]
        fn daylight_saving_time_offset(&self) -> f64 {
            if self.ivars().zone.at(crate::date::now()).dst { 3600.0 } else { 0.0 }
        }

        #[unsafe(method(daylightSavingTimeOffsetForDate:))]
        fn daylight_saving_time_offset_for_date(&self, date: &NSDate) -> f64 {
            if self.ivars().zone.at(crate::date::time_of(date)).dst { 3600.0 } else { 0.0 }
        }

        #[unsafe(method_id(nextDaylightSavingTimeTransition))]
        fn next_daylight_saving_time_transition(&self) -> Option<Retained<NSDate>> {
            self.ivars().zone.next_transition(crate::date::now()).map(NSDate::dateWithTimeIntervalSinceReferenceDate)
        }

        #[unsafe(method_id(nextDaylightSavingTimeTransitionAfterDate:))]
        fn next_transition_after(&self, date: &NSDate) -> Option<Retained<NSDate>> {
            self.ivars().zone.next_transition(crate::date::time_of(date)).map(NSDate::dateWithTimeIntervalSinceReferenceDate)
        }

        #[unsafe(method_id(localizedName:locale:))]
        fn localized_name(&self, style: NSInteger, _locale: Option<&AnyObject>) -> Option<Retained<NSString>> {
            let zone = &self.ivars().zone;
            let now = crate::date::now();
            // Standard, short standard, DST, short DST, generic, short generic.
            let text = match style {
                0 | 2 => zone.long_name(now),
                4 => zone.generic_long(now),
                5 => zone.generic_short(now),
                _ => zone.short_specific(now),
            };
            Some(NSString::from_str(&text))
        }

        #[unsafe(method(isEqualToTimeZone:))]
        fn is_equal_to_time_zone(&self, other: &NSTimeZone) -> bool {
            zone_of(other).name == self.ivars().zone.name
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSTimeZone>()).is_some_and(|o| zone_of(o).name == self.ivars().zone.name)
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            crate::string::hash_str(&self.ivars().zone.name)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let zone = &self.ivars().zone;
            let moment = zone.at(crate::date::now());
            let dst = if moment.dst { " (Daylight)" } else { "" };
            NSString::from_str(&format!("{} ({}) offset {}{dst}", zone.name, moment.abbreviation, moment.offset))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSTimeZoneImpl {}
);

/// The zone a time zone object holds.
pub(crate) fn zone_of(zone: &NSTimeZone) -> &Zone {
    // SAFETY: every NSTimeZone is an instance of this class.
    &unsafe { &*(zone as *const NSTimeZone).cast::<NSTimeZoneImpl>() }.ivars().zone
}

fn init_zone(this: Allocated<NSTimeZoneImpl>, zone: Option<Zone>) -> Option<Retained<NSTimeZoneImpl>> {
    match zone {
        Some(zone) => {
            let this = this.set_ivars(ZoneIvars { zone });
            // SAFETY: NSObject's designated initializer.
            Some(unsafe { msg_send![super(this), init] })
        }
        None => {
            drop(this);
            None
        }
    }
}

/// A new time zone object.
pub(crate) fn make(zone: Zone) -> Retained<NSTimeZoneImpl> {
    // SAFETY: +alloc through the binding loads the class.
    let this: Allocated<NSTimeZoneImpl> = unsafe { msg_send![NSTimeZone::class(), alloc] };
    init_zone(this, Some(zone)).expect("a zone")
}

/// A time zone object for Rust code.
pub(crate) fn object(zone: Zone) -> Retained<NSTimeZone> {
    // SAFETY: NSTimeZoneImpl is the class NSTimeZone names.
    unsafe { Retained::cast_unchecked(make(zone)) }
}

fn system_cache() -> &'static Mutex<Option<Zone>> {
    static SYSTEM: OnceLock<Mutex<Option<Zone>>> = OnceLock::new();
    SYSTEM.get_or_init(Default::default)
}

fn default_cache() -> &'static Mutex<Option<Zone>> {
    static DEFAULT: OnceLock<Mutex<Option<Zone>>> = OnceLock::new();
    DEFAULT.get_or_init(Default::default)
}

/// The system zone, looked up once until reset.
pub(crate) fn system_zone() -> Zone {
    crate::thread::lock(system_cache()).get_or_insert_with(Zone::system).clone()
}

/// The default zone: set, or the system's.
pub(crate) fn default_zone() -> Zone {
    crate::thread::lock(default_cache()).clone().unwrap_or_else(system_zone)
}

/// The zone macOS maps a common abbreviation to.
fn zone_for_abbreviation(abbreviation: &str) -> Option<&'static str> {
    Some(match abbreviation {
        "GMT" | "UTC" => "GMT",
        "EST" | "EDT" => "America/New_York",
        "CST" | "CDT" => "America/Chicago",
        "MST" | "MDT" => "America/Denver",
        "PST" | "PDT" => "America/Los_Angeles",
        "AKST" | "AKDT" => "America/Anchorage",
        "HST" => "Pacific/Honolulu",
        "BST" => "Europe/London",
        "CET" | "CEST" => "Europe/Paris",
        "EET" | "EEST" => "Europe/Athens",
        "WET" | "WEST" => "Europe/Lisbon",
        "IST" => "Asia/Kolkata",
        "JST" => "Asia/Tokyo",
        "KST" => "Asia/Seoul",
        "HKT" => "Asia/Hong_Kong",
        "SGT" => "Asia/Singapore",
        "NZST" | "NZDT" => "Pacific/Auckland",
        "AEST" | "AEDT" => "Australia/Sydney",
        "MSK" => "Europe/Moscow",
        "BRT" => "America/Sao_Paulo",
        "ART" => "America/Argentina/Buenos_Aires",
        _ => return None,
    })
}

/// The names in the tz database: region directories' zone files.
#[cfg_attr(not(feature = "collections"), allow(dead_code))]
fn known_names() -> Vec<String> {
    fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let full = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if path.is_dir() {
                walk(&path, &full, out);
            } else if !prefix.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                out.push(full);
            }
        }
    }
    let mut out = Vec::new();
    for region in
        ["Africa", "America", "Antarctica", "Arctic", "Asia", "Atlantic", "Australia", "Europe", "Indian", "Pacific"]
    {
        walk(&std::path::Path::new("/usr/share/zoneinfo").join(region), region, &mut out);
    }
    out.push("GMT".into());
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones() {
        let september = 780_000_000.5;
        let new_york = Zone::named("America/New_York").unwrap();
        let moment = new_york.at(september);
        assert_eq!((moment.offset, moment.dst, moment.abbreviation.as_str()), (-14400, true, "EDT"));
        assert_eq!(new_york.long_name(september), "Eastern Daylight Time");
        assert_eq!(new_york.short_specific(september), "EDT");
        assert_eq!(
            (new_york.generic_short(september), new_york.generic_long(september)),
            ("ET".into(), "Eastern Time".into())
        );
        assert_eq!(
            (new_york.exemplar_city(), new_york.generic_location(september)),
            ("New York".into(), "New York Time".into())
        );
        let paris = Zone::named("Europe/Paris").unwrap();
        assert_eq!(
            (paris.short_specific(september), paris.long_name(september)),
            ("GMT+2".into(), "Central European Summer Time".into())
        );
        let january = 757_000_000.0;
        assert_eq!(new_york.at(january).abbreviation, "EST");
        assert_eq!(new_york.next_transition(january), Some(763_196_400.0));
        let fixed = Zone::fixed(19800);
        assert_eq!(fixed.name, "GMT+0530");
        assert_eq!(fixed.at(0.0).abbreviation, "GMT+5:30");
        assert_eq!(Zone::fixed(0).name, "GMT");
        assert_eq!(Zone::named("UTC").unwrap().name, "GMT");
        assert!(Zone::named("Nowhere/Nope").is_none());
        assert_eq!(short_gmt(-3600), "GMT-1");
        assert_eq!(long_gmt(19800), "GMT+05:30");
        assert_eq!(long_gmt(0), "GMT+00:00");
    }
}

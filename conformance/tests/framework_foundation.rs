//! Public API contracts used by Upleft. macOS execution remains unverified.
use objc2::AnyThread;
use objc2::rc::autoreleasepool;
use objc2_foundation::*;
use sidestep as _;

#[test]
fn bounded_file_reads_seek_write_close_and_errors() {
    autoreleasepool(|_| {
        let path = std::env::temp_dir().join(format!("sidestep-handle-{}", std::process::id()));
        std::fs::write(&path, b"abcdef").unwrap();
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
        let handle = NSFileHandle::fileHandleForUpdatingURL_error(&url).unwrap();
        assert_eq!(handle.readDataUpToLength_error(2).unwrap().to_vec(), b"ab");
        assert_eq!(handle.readDataUpToLength_error(2).unwrap().to_vec(), b"cd");
        handle.seekToOffset_error(1).unwrap();
        handle.writeData_error(&NSData::with_bytes(b"X")).unwrap();
        handle.seekToOffset_error(0).unwrap();
        assert_eq!(handle.readDataToEndOfFileAndReturnError().unwrap().to_vec(), b"aXcdef");
        assert!(handle.readDataUpToLength_error(2).unwrap().is_empty());
        handle.closeAndReturnError().unwrap();
        assert!(handle.readDataUpToLength_error(1).is_err());
        std::fs::remove_file(path).unwrap();
        assert!(NSFileHandle::fileHandleForReadingFromURL_error(&url).is_err());
    });
}

#[test]
fn calendar_addition_preserves_wall_clock_across_dst() {
    autoreleasepool(|_| {
        let calendar =
            NSCalendar::initWithCalendarIdentifier(NSCalendar::alloc(), unsafe { NSCalendarIdentifierGregorian })
                .unwrap();
        calendar.setTimeZone(&NSTimeZone::timeZoneWithName(&NSString::from_str("America/New_York")).unwrap());
        // Noon EST, the day before the 2024 spring transition.
        let date = NSDate::dateWithTimeIntervalSince1970(1_710_003_600.0);
        assert_eq!(calendar.component_fromDate(NSCalendarUnit::Hour, &date), 12);
        let next = calendar
            .dateByAddingUnit_value_toDate_options(NSCalendarUnit::Day, 1, &date, NSCalendarOptions::empty())
            .unwrap();
        assert_eq!(next.timeIntervalSinceDate(&date), 23.0 * 3600.0);
        assert_eq!(calendar.component_fromDate(NSCalendarUnit::Hour, &next), 12);
        let start = calendar.startOfDayForDate(&next);
        assert_eq!(calendar.component_fromDate(NSCalendarUnit::Hour, &start), 0);
        assert!(calendar.isDate_inSameDayAsDate(&start, &next));
        assert!(!calendar.isDate_inSameDayAsDate(&date, &next));
        let copied = calendar.copy();
        copied.setFirstWeekday(2);
        calendar.setFirstWeekday(1);
        assert_eq!(copied.firstWeekday(), 2);
    });
}

/// Calendars by their identifiers' values, and a URL session's sizes and
/// priorities.
#[test]
fn calendar_identifiers_and_session_constants() {
    let identifiers = unsafe {
        [
            (NSCalendarIdentifierGregorian, "gregorian"),
            (NSCalendarIdentifierBuddhist, "buddhist"),
            (NSCalendarIdentifierChinese, "chinese"),
            (NSCalendarIdentifierCoptic, "coptic"),
            (NSCalendarIdentifierEthiopicAmeteMihret, "ethiopic"),
            (NSCalendarIdentifierEthiopicAmeteAlem, "ethiopic-amete-alem"),
            (NSCalendarIdentifierHebrew, "hebrew"),
            (NSCalendarIdentifierISO8601, "iso8601"),
            (NSCalendarIdentifierIndian, "indian"),
            (NSCalendarIdentifierIslamic, "islamic"),
            (NSCalendarIdentifierIslamicCivil, "islamic-civil"),
            (NSCalendarIdentifierJapanese, "japanese"),
            (NSCalendarIdentifierPersian, "persian"),
            (NSCalendarIdentifierRepublicOfChina, "roc"),
            (NSCalendarIdentifierIslamicTabular, "islamic-tbla"),
            (NSCalendarIdentifierIslamicUmmAlQura, "islamic-umalqura"),
        ]
    };
    for (identifier, value) in identifiers {
        assert_eq!(identifier.to_string(), value);
    }
    assert_eq!(unsafe { NSURLSessionTransferSizeUnknown }, -1);
    let priorities =
        unsafe { (NSURLSessionTaskPriorityLow, NSURLSessionTaskPriorityDefault, NSURLSessionTaskPriorityHigh) };
    assert_eq!(priorities, (0.25, 0.5, 0.75));
}

#[test]
fn number_formatter_localized_roundtrip_and_range() {
    autoreleasepool(|_| {
        let f = NSNumberFormatter::new();
        f.setLocale(Some(&NSLocale::initWithLocaleIdentifier(NSLocale::alloc(), &NSString::from_str("de_DE"))));
        f.setNumberStyle(NSNumberFormatterStyle::DecimalStyle);
        f.setMaximumFractionDigits(2);
        assert_eq!(f.stringFromNumber(&NSNumber::numberWithDouble(1234.5)).unwrap().to_string(), "1.234,5");
        assert_eq!(f.numberFromString(&NSString::from_str("1.234,5")).unwrap().doubleValue(), 1234.5);
        f.setUsesGroupingSeparator(false);
        f.setMinimum(Some(&NSNumber::numberWithInt(1)));
        f.setMaximum(Some(&NSNumber::numberWithInt(10)));
        assert!(f.numberFromString(&NSString::from_str("11")).is_none());
        assert!(f.numberFromString(&NSString::from_str("5 trailing")).is_none());
        assert_eq!(f.numberFromString(&NSString::from_str("5,5")).unwrap().doubleValue(), 5.5);
        f.setAllowsFloats(false);
        assert!(f.numberFromString(&NSString::from_str("5,5")).is_none());
    });
}

#[test]
fn byte_count_binary_decimal_and_relative_dates() {
    autoreleasepool(|_| {
        let f = NSByteCountFormatter::new();
        f.setAllowsNonnumericFormatting(false);
        f.setCountStyle(NSByteCountFormatterCountStyle::Decimal);
        let decimal = f.stringFromByteCount(1_000_000).to_string();
        assert!(decimal.contains('1') && decimal.contains("MB"), "{decimal}");
        f.setCountStyle(NSByteCountFormatterCountStyle::Binary);
        let binary = f.stringFromByteCount(1_048_576).to_string();
        assert!(binary.contains('1') && binary.contains("MB"), "{binary}");
        let relative = NSRelativeDateTimeFormatter::new();
        // The date formatter's output is locale dependent; it must be nonempty and distinguish directions.
        let before = relative.localizedStringFromTimeInterval(-3600.0).to_string();
        let after = relative.localizedStringFromTimeInterval(3600.0).to_string();
        assert!(!before.is_empty());
        assert!(!after.is_empty());
        assert_ne!(before, after);
    });
}

#[test]
fn data_compression_roundtrip_and_truncated_stream() {
    autoreleasepool(|_| {
        let bytes = b"A document snapshot. A document snapshot. A document snapshot.";
        let data = NSData::with_bytes(bytes);
        let compressed = data.compressedDataUsingAlgorithm_error(NSDataCompressionAlgorithm::Zlib).unwrap();
        assert!(compressed.length() < data.length());
        assert_eq!(
            compressed.decompressedDataUsingAlgorithm_error(NSDataCompressionAlgorithm::Zlib).unwrap().to_vec(),
            bytes
        );
        let mut truncated = compressed.to_vec();
        truncated.truncate(truncated.len() / 2);
        assert!(
            NSData::with_bytes(&truncated)
                .decompressedDataUsingAlgorithm_error(NSDataCompressionAlgorithm::Zlib)
                .is_err()
        );
        // A raw DEFLATE stored block for "abc" (RFC 1951), independent of the encoder.
        let fixture = NSData::with_bytes(&[1, 3, 0, 252, 255, b'a', b'b', b'c']);
        assert_eq!(
            fixture.decompressedDataUsingAlgorithm_error(NSDataCompressionAlgorithm::Zlib).unwrap().to_vec(),
            b"abc"
        );
    });
}

#[test]
fn directory_enumeration_skips_descendants_and_does_not_follow_symlinks() {
    autoreleasepool(|_| {
        let path = std::env::temp_dir().join(format!("sidestep-enumeration-{}", std::process::id()));
        std::fs::create_dir_all(path.join("child")).unwrap();
        std::fs::write(path.join("child/nested.txt"), b"nested").unwrap();
        std::fs::write(path.join("visible.txt"), b"visible").unwrap();
        std::fs::write(path.join(".hidden"), b"hidden").unwrap();
        std::os::unix::fs::symlink(&path, path.join("cycle")).unwrap();
        let root = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
        let enumerator = NSFileManager::defaultManager()
            .enumeratorAtURL_includingPropertiesForKeys_options_errorHandler(
                &root,
                None,
                NSDirectoryEnumerationOptions::SkipsHiddenFiles,
                None,
            )
            .unwrap();
        let mut names = Vec::new();
        while let Some(item) = enumerator.nextObject() {
            let name = item.lastPathComponent().unwrap().to_string();
            if name == "child" {
                assert_eq!(enumerator.level(), 1);
                enumerator.skipDescendants();
            }
            names.push(name);
        }
        names.sort();
        assert_eq!(names, ["child", "cycle", "visible.txt"]);
        let enumerator = NSFileManager::defaultManager()
            .enumeratorAtURL_includingPropertiesForKeys_options_errorHandler(
                &root,
                None,
                NSDirectoryEnumerationOptions::empty(),
                None,
            )
            .unwrap();
        assert_eq!(enumerator.allObjects().count(), 5);
        std::fs::remove_dir_all(path).unwrap();
    });
}

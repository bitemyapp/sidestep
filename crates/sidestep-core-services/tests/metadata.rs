#![cfg(target_os = "linux")]
#![allow(deprecated)]
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFString, CFType};
use objc2_core_services::*;
use sidestep_core_services as _;

#[test]
fn metadata_values_are_live_and_missing_attributes_stay_missing() {
    let directory = std::env::temp_dir().join(format!("sidestep-metadata-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("sample.md");
    std::fs::write(&path, "hello").unwrap();
    let item = unsafe { MDItemCreate(None, Some(&CFString::from_str(path.to_str().unwrap()))) }.unwrap();
    let size = || {
        unsafe { MDItemCopyAttribute(&item, kMDItemFSSize) }.unwrap().downcast::<CFNumber>().unwrap().as_i64().unwrap()
    };
    assert_eq!(size(), 5);
    std::fs::write(&path, "hello world").unwrap();
    assert_eq!(size(), 11);
    let name = unsafe { MDItemCopyAttribute(&item, kMDItemFSName) }.unwrap().downcast::<CFString>().unwrap();
    assert_eq!(name.to_string(), "sample.md");
    let keys = CFArray::from_retained_objects(&[
        CFString::from_str("kMDItemFSName"),
        CFString::from_str("kMDItemFSSize"),
        CFString::from_str("unknown"),
    ]);
    let values = unsafe { MDItemCopyAttributes(&item, Some(keys.as_ref())) }.unwrap();
    let values = unsafe { &*(&*values as *const CFDictionary).cast::<CFDictionary<CFString, CFType>>() };
    assert_eq!(values.len(), 2);
    assert!(values.get(&CFString::from_str("unknown")).is_none());
    let names = unsafe { MDItemCopyAttributeNames(&item) }.unwrap();
    let names = unsafe { &*(&*names as *const CFArray).cast::<CFArray<CFString>>() };
    assert!(names.to_vec().iter().any(|s| s.to_string() == "kMDItemFSSize"));
    std::fs::remove_file(path).unwrap();
    assert!(unsafe { MDItemCopyAttribute(&item, kMDItemFSSize) }.is_none());
    std::fs::remove_dir(directory).unwrap();
}

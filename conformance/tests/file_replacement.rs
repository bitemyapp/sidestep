//! FileManager replacement: published API, real files, owned output URLs.
use objc2::rc::Retained;
use objc2_foundation::{NSFileManager, NSFileManagerItemReplacementOptions as Options, NSString, NSURL};
use sidestep as _;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
static SERIAL: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "sidestep-replace-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn url(&self, name: &str) -> Retained<NSURL> {
        NSURL::fileURLWithPath(&NSString::from_str(self.0.join(name).to_str().unwrap()))
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn replaces_contents_consumes_source_and_preserves_permissions() {
    let f = Fixture::new();
    f.write("document", b"old");
    f.write("new", b"new");
    std::fs::set_permissions(f.0.join("document"), std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::set_permissions(f.0.join("new"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut result = None;
    NSFileManager::defaultManager()
        .replaceItemAtURL_withItemAtURL_backupItemName_options_resultingItemURL_error(
            &f.url("document"),
            &f.url("new"),
            None,
            Options::empty(),
            Some(&mut result),
        )
        .unwrap();
    assert_eq!(std::fs::read(f.0.join("document")).unwrap(), b"new");
    assert!(!f.0.join("new").exists());
    assert_eq!(std::fs::metadata(f.0.join("document")).unwrap().permissions().mode() & 0o777, 0o640);
    assert_eq!(result.unwrap().path().unwrap().to_string(), f.0.join("document").to_str().unwrap());
}
#[test]
fn backup_and_new_metadata_options_are_honored() {
    let f = Fixture::new();
    f.write("document", b"old");
    f.write("new", b"new");
    std::fs::set_permissions(f.0.join("new"), std::fs::Permissions::from_mode(0o600)).unwrap();
    NSFileManager::defaultManager()
        .replaceItemAtURL_withItemAtURL_backupItemName_options_resultingItemURL_error(
            &f.url("document"),
            &f.url("new"),
            Some(&NSString::from_str("backup")),
            Options::UsingNewMetadataOnly | Options::WithoutDeletingBackupItem,
            None,
        )
        .unwrap();
    assert_eq!(std::fs::read(f.0.join("backup")).unwrap(), b"old");
    assert_eq!(std::fs::metadata(f.0.join("document")).unwrap().permissions().mode() & 0o777, 0o600);
    f.write("new", b"third");
    NSFileManager::defaultManager()
        .replaceItemAtURL_withItemAtURL_backupItemName_options_resultingItemURL_error(
            &f.url("document"),
            &f.url("new"),
            Some(&NSString::from_str("temporary-backup")),
            Options::empty(),
            None,
        )
        .unwrap();
    assert!(!f.0.join("temporary-backup").exists());
    assert_eq!(std::fs::read(f.0.join("document")).unwrap(), b"third");
}
#[test]
fn missing_replacement_leaves_original_intact() {
    let f = Fixture::new();
    f.write("document", b"old");
    assert!(
        NSFileManager::defaultManager()
            .replaceItemAtURL_withItemAtURL_backupItemName_options_resultingItemURL_error(
                &f.url("document"),
                &f.url("absent"),
                None,
                Options::empty(),
                None
            )
            .is_err()
    );
    assert_eq!(std::fs::read(f.0.join("document")).unwrap(), b"old");
}
#[test]
fn replaces_nonempty_directory_without_merging_contents() {
    let f = Fixture::new();
    std::fs::create_dir(f.0.join("old")).unwrap();
    std::fs::create_dir(f.0.join("new")).unwrap();
    f.write("old/old-child", b"old");
    f.write("new/new-child", b"new");
    NSFileManager::defaultManager()
        .replaceItemAtURL_withItemAtURL_backupItemName_options_resultingItemURL_error(
            &f.url("old"),
            &f.url("new"),
            None,
            Options::empty(),
            None,
        )
        .unwrap();
    assert_eq!(std::fs::read(f.0.join("old/new-child")).unwrap(), b"new");
    assert!(!f.0.join("old/old-child").exists());
    assert!(!f.0.join("new").exists());
}

//! UniformTypeIdentifiers queried through its public Objective-C bindings.
use objc2::Message;
use objc2_foundation::{NSCopying, NSObjectProtocol, NSString};
use objc2_uniform_type_identifiers::*;
use sidestep as _;
#[test]
fn constants_are_objects_with_transitive_conformance() {
    let png = unsafe { UTTypePNG };
    let image = unsafe { UTTypeImage };
    let data = unsafe { UTTypeData };
    assert_eq!(png.identifier().to_string(), "public.png");
    assert!(png.conformsToType(image));
    assert!(png.conformsToType(data));
    assert!(png.conformsToType(png));
    assert!(!png.isSubtypeOfType(png));
    assert!(image.isSupertypeOfType(png));
    assert!(!data.isSubtypeOfType(png));
    assert!(png.supertypes().containsObject(image));
    assert!(png.supertypes().containsObject(data));
    assert!(png.isDeclared());
    assert!(png.isPublicType());
    assert!(!png.isDynamic());
    let retained = png.retain();
    let copy = png.copy();
    assert!(copy.isEqual(Some(&retained)));
}
#[test]
fn factories_and_tags_agree() {
    let jpeg = UTType::typeWithFilenameExtension(&NSString::from_str("JPG")).unwrap();
    assert!(jpeg.isEqual(Some(unsafe { UTTypeJPEG })));
    assert_eq!(jpeg.preferredMIMEType().unwrap().to_string(), "image/jpeg");
    let mime = UTType::typeWithMIMEType(&NSString::from_str("image/jpeg")).unwrap();
    assert!(jpeg.isEqual(Some(&mime)));
    let pdf = UTType::typeWithIdentifier(&NSString::from_str("com.adobe.pdf")).unwrap();
    assert_eq!(pdf.preferredFilenameExtension().unwrap().to_string(), "pdf");
    assert!(pdf.conformsToType(unsafe { UTTypeData }));
    let tags = jpeg.tags();
    let extensions = tags.objectForKey(unsafe { UTTagClassFilenameExtension }).unwrap();
    assert!(extensions.containsObject(&NSString::from_str("jpg")));
    assert!(UTType::typeWithIdentifier(&NSString::from_str("invalid.type.sidestep-unknown")).is_none());
}
#[test]
fn unknown_extension_preserves_tag_and_requested_parent() {
    let tag = NSString::from_str("sidestep-conformance-unknown-extension");
    let ty = UTType::typeWithFilenameExtension_conformingToType(&tag, unsafe { UTTypeData }).unwrap();
    assert!(ty.isDynamic());
    assert!(!ty.isDeclared());
    assert!(ty.conformsToType(unsafe { UTTypeData }));
    assert_eq!(ty.preferredFilenameExtension().as_deref(), Some(&*tag));
    let again = UTType::typeWithIdentifier(&ty.identifier()).unwrap();
    assert!(again.isEqual(Some(&ty)));
    assert!(ty.copy().isEqual(Some(&ty)));
}

#[test]
fn main_bundle_type_declarations() {
    let root = std::env::temp_dir().join(format!("sidestep-uti-bundle-{}.app", std::process::id()));
    let contents = root.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    let binary = contents.join("MacOS/type-probe");
    std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>org.sidestep.conformance.types</string>
<key>CFBundleExecutable</key><string>type-probe</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>UTExportedTypeDeclarations</key><array><dict>
<key>UTTypeIdentifier</key><string>org.sidestep.conformance.document</string>
<key>UTTypeDescription</key><string>Sidestep conformance document</string>
<key>UTTypeConformsTo</key><array><string>public.text</string></array>
<key>UTTypeTagSpecification</key><dict>
<key>public.filename-extension</key><array><string>sidestepfixture</string><string>sidestepfixture2</string></array>
<key>public.mime-type</key><string>application/x-sidestep-conformance</string>
</dict></dict></array></dict></plist>"#,
    )
    .unwrap();
    let result = std::process::Command::new(binary)
        .args(["--exact", "declared_type_child", "--nocapture"])
        .env("SIDESTEP_UTI_CHILD", "1")
        .output()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[test]
fn declared_type_child() {
    if std::env::var_os("SIDESTEP_UTI_CHILD").is_none() {
        return;
    }
    let ty = UTType::typeWithIdentifier(&NSString::from_str("org.sidestep.conformance.document")).unwrap();
    assert!(ty.isDeclared());
    assert!(!ty.isDynamic());
    assert!(!ty.isPublicType());
    assert!(ty.conformsToType(unsafe { UTTypeText }));
    let ext = UTType::typeWithFilenameExtension(&NSString::from_str("sidestepfixture2")).unwrap();
    assert!(ty.isEqual(Some(&ext)));
    assert_eq!(ty.preferredMIMEType().unwrap().to_string(), "application/x-sidestep-conformance");
}

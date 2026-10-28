//! UniformTypeIdentifiers objects, common document types and tag matching.
#![cfg(not(target_vendor = "apple"))]
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSCopying, NSDictionary, NSNumber, NSSet, NSString, NSURL, NSZone};
use sidestep_runtime::{ObjectRef, StaticObject};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex};

sidestep_runtime::static_class!(pub UTTYPE, UTTYPE_META = "UTType", || { let _ = Type::class(); });
sidestep_foundation::constant_string!(UTTagClassFilenameExtension = "public.filename-extension");
sidestep_foundation::constant_string!(UTTagClassMIMEType = "public.mime-type");
const EXT: &str = "public.filename-extension";
const MIME: &str = "public.mime-type";

struct Builtin {
    id: &'static str,
    parents: &'static [&'static str],
    extensions: &'static [&'static str],
    mime: &'static [&'static str],
    description: &'static str,
}
#[derive(Clone)]
pub struct Data {
    id: String,
    parents: Vec<String>,
    extensions: Vec<String>,
    mime: Vec<String>,
    description: String,
    declared: bool,
}
impl From<&Builtin> for Data {
    fn from(b: &Builtin) -> Self {
        let strings = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        Self {
            id: b.id.into(),
            parents: strings(b.parents),
            extensions: strings(b.extensions),
            mime: strings(b.mime),
            description: b.description.into(),
            declared: true,
        }
    }
}
static CUSTOM: LazyLock<Mutex<HashMap<String, Data>>> = LazyLock::new(|| Mutex::new(bundle_types()));

fn bundle_types() -> HashMap<String, Data> {
    type Dict = NSDictionary;
    fn field(d: &Dict, key: &str) -> Option<Retained<AnyObject>> {
        d.objectForKey(&*NSString::from_str(key))
    }
    fn list(value: Option<Retained<AnyObject>>) -> Vec<String> {
        let Some(value) = value else { return Vec::new() };
        if let Some(s) = value.downcast_ref::<NSString>() {
            return vec![s.to_string()];
        }
        value
            .downcast_ref::<NSArray<AnyObject>>()
            .map(|a| a.iter().filter_map(|o| o.downcast_ref::<NSString>().map(ToString::to_string)).collect())
            .unwrap_or_default()
    }
    let mut types = HashMap::new();
    let Some(info) = NSBundle::mainBundle().infoDictionary() else { return types };
    let info = (info.as_ref() as &AnyObject).downcast_ref::<Dict>().expect("bundle dictionary");
    for key in ["UTExportedTypeDeclarations", "UTImportedTypeDeclarations"] {
        let Some(declarations) = field(info, key) else { continue };
        let Some(declarations) = declarations.downcast_ref::<NSArray<AnyObject>>() else { continue };
        for declaration in declarations {
            let Some(d) = declaration.downcast_ref::<Dict>() else { continue };
            let Some(id) = list(field(d, "UTTypeIdentifier")).into_iter().next() else { continue };
            let tags = field(d, "UTTypeTagSpecification");
            let tags = tags.as_deref().and_then(|o| o.downcast_ref::<Dict>());
            let data = Data {
                id: id.clone(),
                parents: list(field(d, "UTTypeConformsTo")),
                extensions: list(tags.and_then(|t| field(t, EXT))),
                mime: list(tags.and_then(|t| field(t, MIME))),
                description: list(field(d, "UTTypeDescription")).into_iter().next().unwrap_or_else(|| id.clone()),
                declared: true,
            };
            types.entry(id).or_insert(data);
        }
    }
    types
}
macro_rules! types {
    ($($symbol:ident: $id:literal, [$($parent:literal),*], [$($ext:literal),*], [$($mime:literal),*], $description:literal;)*) => {
        $(#[allow(non_upper_case_globals)] #[unsafe(no_mangle)] pub static $symbol: ObjectRef = {
            static OBJECT: StaticObject<Builtin> = StaticObject::new(&UTTYPE, Builtin {
                id:$id, parents:&[$($parent),*], extensions:&[$($ext),*], mime:&[$($mime),*], description:$description,
            });
            OBJECT.object_ref()
        };)*
        fn builtin(id: &str) -> Option<Retained<Type>> {
            $(if id == $id {
                // SAFETY: the exported pointer names an immortal UTType object.
                return unsafe { Retained::retain((&raw const $symbol).cast::<*mut Type>().read()) };
            })*
            None
        }
        const IDS: &[&str] = &[$($id),*];
    }
}
types! {
    UTTypeItem: "public.item", [], [], [], "Item";
    UTTypeContent: "public.content", [], [], [], "Content";
    UTTypeData: "public.data", ["public.item"], [], [], "Data";
    UTTypeDirectory: "public.directory", ["public.item"], [], [], "Directory";
    UTTypeFolder: "public.folder", ["public.directory"], [], [], "Folder";
    UTTypeVolume: "public.volume", ["public.folder"], [], [], "Volume";
    UTTypePackage: "com.apple.package", ["public.directory"], [], [], "Package";
    UTTypeBundle: "com.apple.bundle", ["public.directory"], ["bundle"], [], "Bundle";
    UTTypeApplication: "com.apple.application", ["public.executable", "public.content"], [], [], "Application";
    UTTypeApplicationBundle: "com.apple.application-bundle", ["com.apple.application", "com.apple.bundle", "com.apple.package"], ["app"], [], "Application";
    UTTypeExecutable: "public.executable", ["public.data"], [], [], "Executable";
    UTTypeText: "public.text", ["public.data", "public.content"], [], ["text/plain"], "Text";
    UTTypePlainText: "public.plain-text", ["public.text"], ["txt", "text"], ["text/plain"], "Plain text";
    _SidestepMarkdown: "net.daringfireball.markdown", ["public.plain-text"], ["md", "markdown", "mdown", "mkd"], ["text/markdown"], "Markdown document";
    UTTypeUTF8PlainText: "public.utf8-plain-text", ["public.plain-text"], [], [], "UTF-8 text";
    UTTypeUTF16PlainText: "public.utf16-plain-text", ["public.plain-text"], [], [], "UTF-16 text";
    UTTypeRTF: "public.rtf", ["public.text"], ["rtf"], ["text/rtf", "application/rtf"], "Rich text";
    UTTypeHTML: "public.html", ["public.text"], ["html", "htm"], ["text/html"], "HTML document";
    UTTypeXML: "public.xml", ["public.text"], ["xml"], ["application/xml", "text/xml"], "XML document";
    UTTypeJSON: "public.json", ["public.text"], ["json"], ["application/json"], "JSON document";
    UTTypeYAML: "public.yaml", ["public.text"], ["yaml", "yml"], ["application/yaml", "text/yaml"], "YAML document";
    UTTypeCommaSeparatedText: "public.comma-separated-values-text", ["public.delimited-values-text"], ["csv"], ["text/csv"], "Comma-separated text";
    UTTypeDelimitedText: "public.delimited-values-text", ["public.text"], [], [], "Delimited text";
    UTTypeSourceCode: "public.source-code", ["public.plain-text"], [], [], "Source code";
    UTTypeScript: "public.script", ["public.source-code"], [], [], "Script";
    UTTypeShellScript: "public.shell-script", ["public.script"], ["sh"], ["text/x-shellscript"], "Shell script";
    UTTypePythonScript: "public.python-script", ["public.script"], ["py"], ["text/x-python"], "Python script";
    UTTypeImage: "public.image", ["public.data", "public.content"], [], [], "Image";
    UTTypePNG: "public.png", ["public.image"], ["png"], ["image/png"], "PNG image";
    UTTypeJPEG: "public.jpeg", ["public.image"], ["jpeg", "jpg", "jpe"], ["image/jpeg"], "JPEG image";
    UTTypeGIF: "com.compuserve.gif", ["public.image"], ["gif"], ["image/gif"], "GIF image";
    UTTypeTIFF: "public.tiff", ["public.image"], ["tiff", "tif"], ["image/tiff"], "TIFF image";
    UTTypeSVG: "public.svg-image", ["public.image", "public.xml"], ["svg"], ["image/svg+xml"], "SVG image";
    UTTypePDF: "com.adobe.pdf", ["public.data", "public.composite-content"], ["pdf"], ["application/pdf"], "PDF document";
    UTTypeCompositeContent: "public.composite-content", ["public.content"], [], [], "Composite content";
    UTTypeArchive: "public.archive", ["public.data"], [], [], "Archive";
    UTTypeZIP: "public.zip-archive", ["public.archive"], ["zip"], ["application/zip"], "ZIP archive";
    UTTypeURL: "public.url", ["public.data"], [], [], "URL";
    UTTypeFileURL: "public.file-url", ["public.url"], [], [], "File URL";
}

fn data(t: &Type) -> Data {
    if let Some(t) = (t as &AnyObject).downcast_ref::<Dynamic>() {
        return t.ivars().clone();
    }
    // SAFETY: base UTType instances are the constant objects above; dynamic
    // instances use the subclass's owned ivars instead.
    Data::from(unsafe { StaticObject::<Builtin>::body_of((t as *const Type).cast()) })
}
fn owned(data: Data) -> Retained<Type> {
    let this = Dynamic::alloc().set_ivars(data);
    let value: Retained<Dynamic> = unsafe { msg_send![super(this, NSObject::class()), init] };
    unsafe { Retained::cast_unchecked(value) }
}
fn lookup(id: &str) -> Option<Retained<Type>> {
    builtin(id).or_else(|| CUSTOM.lock().unwrap().get(id).cloned().map(owned))
}
fn conforms(id: &str, parent: &str, seen: &mut HashSet<String>) -> bool {
    id == parent
        || (seen.insert(id.to_owned())
            && lookup(id).is_some_and(|t| data(&t).parents.iter().any(|p| conforms(p, parent, seen))))
}
fn matching(tag: &str, class: &str, parent: Option<&Type>) -> Vec<Retained<Type>> {
    if tag.is_empty() || !matches!(class, EXT | MIME) {
        return Vec::new();
    }
    let tag = tag.to_ascii_lowercase();
    let parent = parent.map(data).map(|d| d.id);
    let mut ids = IDS.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    ids.extend(CUSTOM.lock().unwrap().keys().cloned());
    let mut matches = ids
        .into_iter()
        .filter_map(|id| lookup(&id))
        .filter(|t| {
            let d = data(t);
            let tags = if class == EXT { &d.extensions } else { &d.mime };
            tags.iter().any(|t| t.eq_ignore_ascii_case(&tag))
                && parent.as_ref().is_none_or(|p| conforms(&d.id, p, &mut HashSet::new()))
        })
        .collect::<Vec<_>>();
    // Prefer a specific format to its abstract supertype for a shared tag.
    matches.sort_by_cached_key(|t| {
        let d = data(t);
        let count = ids_of_supertypes(&d).len();
        (std::cmp::Reverse(count), d.id)
    });
    if matches.is_empty() {
        let parent = parent.unwrap_or_else(|| "public.data".into());
        let encode = |s: &str| s.bytes().map(|b| format!("{b:02x}")).collect::<String>();
        let id = format!("dyn.sidestep.{}.{}.{}", encode(&parent), encode(class), encode(&tag));
        let d = Data {
            id: id.clone(),
            parents: vec![parent],
            extensions: if class == EXT { vec![tag.clone()] } else { vec![] },
            mime: if class == MIME { vec![tag.clone()] } else { vec![] },
            description: tag,
            declared: false,
        };
        CUSTOM.lock().unwrap().insert(id, d.clone());
        matches.push(owned(d));
    }
    matches
}
fn strings(values: &[String]) -> Retained<NSArray<NSString>> {
    NSArray::from_retained_slice(&values.iter().map(|v| NSString::from_str(v)).collect::<Vec<_>>())
}
fn ids_of_supertypes(d: &Data) -> HashSet<String> {
    let mut pending = d.parents.clone();
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if id != d.id
            && seen.insert(id.clone())
            && let Some(t) = lookup(&id)
        {
            pending.extend(data(&t).parents);
        }
    }
    seen
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "UTType"]
    pub struct Type;
    impl Type {
        // UTType instances come from its factories; +new must not make an
        // object lacking the immutable descriptor stored by those factories.
        #[unsafe(method_id(init))]
        fn init(_this: Allocated<Self>) -> Option<Retained<Self>> {
            None
        }
        #[unsafe(method_id(typeWithIdentifier:))]
        fn identifier_type(id: &NSString) -> Option<Retained<Self>> {
            lookup(&id.to_string())
        }
        #[unsafe(method_id(typeWithFilenameExtension:))]
        fn extension_type(ext: &NSString) -> Option<Retained<Self>> {
            matching(&ext.to_string(), EXT, None).into_iter().next()
        }
        #[unsafe(method_id(typeWithFilenameExtension:conformingToType:))]
        fn extension_parent(ext: &NSString, parent: &Type) -> Option<Retained<Self>> {
            matching(&ext.to_string(), EXT, Some(parent)).into_iter().next()
        }
        #[unsafe(method_id(typeWithMIMEType:))]
        fn mime_type(mime: &NSString) -> Option<Retained<Self>> {
            matching(&mime.to_string(), MIME, None).into_iter().next()
        }
        #[unsafe(method_id(typeWithMIMEType:conformingToType:))]
        fn mime_parent(mime: &NSString, parent: &Type) -> Option<Retained<Self>> {
            matching(&mime.to_string(), MIME, Some(parent)).into_iter().next()
        }
        #[unsafe(method_id(typeWithTag:tagClass:conformingToType:))]
        fn tagged(tag: &NSString, class: &NSString, parent: Option<&Type>) -> Option<Retained<Self>> {
            matching(&tag.to_string(), &class.to_string(), parent).into_iter().next()
        }
        #[unsafe(method_id(typesWithTag:tagClass:conformingToType:))]
        fn all_tagged(tag: &NSString, class: &NSString, parent: Option<&Type>) -> Retained<NSArray<Self>> {
            NSArray::from_retained_slice(&matching(&tag.to_string(), &class.to_string(), parent))
        }
        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Retained<NSString> {
            NSString::from_str(&data(self).id)
        }
        #[unsafe(method_id(preferredFilenameExtension))]
        fn extension(&self) -> Option<Retained<NSString>> {
            data(self).extensions.first().map(|s| NSString::from_str(s))
        }
        #[unsafe(method_id(preferredMIMEType))]
        fn mime(&self) -> Option<Retained<NSString>> {
            data(self).mime.first().map(|s| NSString::from_str(s))
        }
        #[unsafe(method_id(localizedDescription))]
        fn description_text(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&data(self).description))
        }
        #[unsafe(method_id(version))]
        fn version(&self) -> Option<Retained<NSNumber>> {
            None
        }
        #[unsafe(method_id(referenceURL))]
        fn reference(&self) -> Option<Retained<NSURL>> {
            None
        }
        #[unsafe(method(isDynamic))]
        fn dynamic(&self) -> bool {
            data(self).id.starts_with("dyn.")
        }
        #[unsafe(method(isDeclared))]
        fn declared(&self) -> bool {
            data(self).declared
        }
        #[unsafe(method(isPublicType))]
        fn public(&self) -> bool {
            data(self).id.starts_with("public.")
        }
        #[unsafe(method(conformsToType:))]
        fn conforms(&self, parent: &Type) -> bool {
            conforms(&data(self).id, &data(parent).id, &mut HashSet::new())
        }
        #[unsafe(method(isSubtypeOfType:))]
        fn subtype(&self, parent: &Type) -> bool {
            data(self).id != data(parent).id && conforms(&data(self).id, &data(parent).id, &mut HashSet::new())
        }
        #[unsafe(method(isSupertypeOfType:))]
        fn supertype(&self, child: &Type) -> bool {
            data(self).id != data(child).id && conforms(&data(child).id, &data(self).id, &mut HashSet::new())
        }
        #[unsafe(method_id(supertypes))]
        fn supertypes(&self) -> Retained<NSSet<Type>> {
            let mut pending = data(self).parents;
            let mut seen = HashSet::new();
            let mut found = Vec::new();
            while let Some(id) = pending.pop() {
                if seen.insert(id.clone())
                    && let Some(t) = lookup(&id)
                {
                    pending.extend(data(&t).parents);
                    found.push(t);
                }
            }
            NSSet::from_retained_slice(&found)
        }
        #[unsafe(method_id(tags))]
        fn tags(&self) -> Retained<NSDictionary<NSString, NSArray<NSString>>> {
            let d = data(self);
            let mut keys = Vec::new();
            let mut values = Vec::new();
            for (key, entries) in [(EXT, d.extensions), (MIME, d.mime)] {
                if !entries.is_empty() {
                    keys.push(NSString::from_str(key));
                    values.push(strings(&entries));
                }
            }
            NSDictionary::from_retained_objects(&keys.iter().map(|k| &**k).collect::<Vec<_>>(), &values)
        }
    }
    unsafe impl NSObjectProtocol for Type {
        #[unsafe(method(isEqual:))]
        fn equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<Type>()).is_some_and(|o| data(self).id == data(o).id)
        }
        #[unsafe(method(hash))]
        fn hash_value(&self) -> usize {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            data(self).id.hash(&mut h);
            h.finish() as usize
        }
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&data(self).id)
        }
    }
    unsafe impl NSCopying for Type {
        #[unsafe(method_id(copyWithZone:))]
        fn copy(&self, _: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }
);
define_class!(
    #[unsafe(super(Type, NSObject))]
    #[name = "_SidestepDynamicUTType"]
    #[ivars = Data]
    pub struct Dynamic;
    unsafe impl NSObjectProtocol for Dynamic {}
);

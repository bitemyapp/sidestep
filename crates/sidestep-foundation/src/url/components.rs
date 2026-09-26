//! `NSURLComponents` and `NSURLQueryItem`.
//!
//! Components are kept apart and percent-encoded; the plain accessors
//! decode on the way out and encode on the way in, each with the set of
//! characters its part of a URL may hold as is (a query keeps `+`, `/`
//! and `?`; a user or password keeps `:`). Unlike `NSURL`, `-host` keeps
//! IPv6 brackets and an empty authority gives an empty host. `-string` is
//! `nil` when the parts can't make a URL: a host with a relative path.

use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSString, NSUInteger, NSURL, NSURLComponents, NSURLQueryItem, NSZone};

use super::parse::{self, HOST_KEEP, ITEM_KEEP, USER_KEEP};

sidestep_runtime::static_class!(
    pub(crate) NSURLCOMPONENTS,
    NSURLCOMPONENTS_META = "NSURLComponents",
    || {
        let _ = NSURLComponentsImpl::class();
        crate::perform::install();
    }
);

sidestep_runtime::static_class!(pub(crate) NSURLQUERYITEM, NSURLQUERYITEM_META = "NSURLQueryItem", || {
    let _ = NSURLQueryItemImpl::class();
    crate::perform::install();
});

/// The parts of a URL, percent-encoded.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Parts {
    scheme: Option<String>,
    user: Option<String>,
    password: Option<String>,
    /// With IPv6 brackets.
    host: Option<String>,
    port: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

impl Parts {
    fn parse(s: &str) -> Option<Parts> {
        let p = parse::parse(s)?;
        let part = |r: &Option<std::ops::Range<usize>>| r.clone().map(|r| s[r].to_string());
        let host = p.host.clone().map(|r| {
            let bracketed = r.start > 0 && s.as_bytes()[r.start - 1] == b'[';
            if bracketed { s[r.start - 1..r.end + 1].to_string() } else { s[r].to_string() }
        });
        Some(Parts {
            scheme: part(&p.scheme),
            user: part(&p.user),
            password: part(&p.password),
            host,
            port: part(&p.port).filter(|p| !p.is_empty()),
            path: s[p.path].to_string(),
            query: part(&p.query),
            fragment: part(&p.fragment),
        })
    }

    /// The URL string, if these parts make one.
    fn string(&self) -> Option<String> {
        let authority = self.host.is_some() || self.user.is_some() || self.password.is_some() || self.port.is_some();
        if authority && !self.path.is_empty() && !self.path.starts_with('/') {
            return None;
        }
        let mut out = String::new();
        if let Some(scheme) = &self.scheme {
            out.push_str(scheme);
            out.push(':');
        }
        if authority {
            out.push_str("//");
            if let Some(user) = &self.user {
                out.push_str(user);
            }
            if let Some(password) = &self.password {
                out.push(':');
                out.push_str(password);
            }
            if self.user.is_some() || self.password.is_some() {
                out.push('@');
            }
            out.push_str(self.host.as_deref().unwrap_or(""));
            if let Some(port) = &self.port {
                out.push(':');
                out.push_str(port);
            }
        }
        out.push_str(&self.path);
        if let Some(query) = &self.query {
            out.push('?');
            out.push_str(query);
        }
        if let Some(fragment) = &self.fragment {
            out.push('#');
            out.push_str(fragment);
        }
        Some(out)
    }
}

pub(crate) struct ComponentsIvars {
    parts: Mutex<Parts>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLComponents"]
    #[ivars = ComponentsIvars]
    pub(crate) struct NSURLComponentsImpl;

    impl NSURLComponentsImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_with(this, Parts::default())
        }

        #[unsafe(method_id(initWithString:))]
        fn init_with_string(this: Allocated<Self>, string: &NSString) -> Option<Retained<Self>> {
            init_parsed(this, &string.to_string(), true)
        }

        #[unsafe(method_id(initWithString:encodingInvalidCharacters:))]
        fn init_with_string_encoding(
            this: Allocated<Self>,
            string: &NSString,
            encode: bool,
        ) -> Option<Retained<Self>> {
            init_parsed(this, &string.to_string(), encode)
        }

        #[unsafe(method_id(initWithURL:resolvingAgainstBaseURL:))]
        fn init_with_url(this: Allocated<Self>, url: &NSURL, resolve: bool) -> Option<Retained<Self>> {
            init_parsed(this, &url_string(url, resolve), false)
        }

        #[unsafe(method_id(componentsWithString:))]
        fn with_string(string: &NSString) -> Option<Retained<Self>> {
            init_parsed(Self::alloc(), &string.to_string(), true)
        }

        #[unsafe(method_id(componentsWithString:encodingInvalidCharacters:))]
        fn with_string_encoding(string: &NSString, encode: bool) -> Option<Retained<Self>> {
            init_parsed(Self::alloc(), &string.to_string(), encode)
        }

        #[unsafe(method_id(componentsWithURL:resolvingAgainstBaseURL:))]
        fn with_url(url: &NSURL, resolve: bool) -> Option<Retained<Self>> {
            init_parsed(Self::alloc(), &url_string(url, resolve), false)
        }

        #[unsafe(method_id(URL))]
        fn url(&self) -> Option<Retained<NSURL>> {
            self.url_with_base(None)
        }

        #[unsafe(method_id(URLRelativeToURL:))]
        fn url_relative_to(&self, base: Option<&NSURL>) -> Option<Retained<NSURL>> {
            self.url_with_base(base)
        }

        #[unsafe(method_id(string))]
        fn string(&self) -> Option<Retained<NSString>> {
            self.parts().string().map(|s| NSString::from_str(&s))
        }

        #[unsafe(method_id(scheme))]
        fn scheme(&self) -> Option<Retained<NSString>> {
            self.parts().scheme.as_deref().map(NSString::from_str)
        }

        #[unsafe(method(setScheme:))]
        fn set_scheme(&self, scheme: Option<&NSString>) {
            self.parts().scheme = scheme.map(|s| s.to_string());
        }

        #[unsafe(method_id(user))]
        fn user(&self) -> Option<Retained<NSString>> {
            decoded(self.parts().user.as_deref())
        }

        #[unsafe(method(setUser:))]
        fn set_user(&self, user: Option<&NSString>) {
            self.parts().user = encoded(user, USER_KEEP);
        }

        #[unsafe(method_id(password))]
        fn password(&self) -> Option<Retained<NSString>> {
            decoded(self.parts().password.as_deref())
        }

        #[unsafe(method(setPassword:))]
        fn set_password(&self, password: Option<&NSString>) {
            self.parts().password = encoded(password, USER_KEEP);
        }

        #[unsafe(method_id(host))]
        fn host(&self) -> Option<Retained<NSString>> {
            decoded(self.parts().host.as_deref())
        }

        #[unsafe(method(setHost:))]
        fn set_host(&self, host: Option<&NSString>) {
            self.parts().host = host.map(|h| encode_host(&h.to_string()));
        }

        #[unsafe(method_id(port))]
        fn port(&self) -> Option<Retained<AnyObject>> {
            let port = self.parts().port.clone();
            port.and_then(|p| super::port_value(&p)).and_then(super::number)
        }

        #[unsafe(method(setPort:))]
        fn set_port(&self, port: Option<&AnyObject>) {
            let port = port.map(|p| {
                // SAFETY: the caller passes an NSNumber.
                let value: isize = unsafe { msg_send![p, integerValue] };
                value.to_string()
            });
            self.parts().port = port;
        }

        #[unsafe(method_id(path))]
        fn path(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&decode_path(&self.parts().path)))
        }

        #[unsafe(method(setPath:))]
        fn set_path(&self, path: Option<&NSString>) {
            self.parts().path = encoded(path, parse::PATH_KEEP).unwrap_or_default();
        }

        #[unsafe(method_id(query))]
        fn query(&self) -> Option<Retained<NSString>> {
            decoded_strict(self.parts().query.as_deref())
        }

        #[unsafe(method(setQuery:))]
        fn set_query(&self, query: Option<&NSString>) {
            self.parts().query = encoded(query, parse::QUERY_KEEP);
        }

        #[unsafe(method_id(fragment))]
        fn fragment(&self) -> Option<Retained<NSString>> {
            decoded_strict(self.parts().fragment.as_deref())
        }

        #[unsafe(method(setFragment:))]
        fn set_fragment(&self, fragment: Option<&NSString>) {
            self.parts().fragment = encoded(fragment, parse::QUERY_KEEP);
        }

        #[unsafe(method_id(percentEncodedUser))]
        fn percent_encoded_user(&self) -> Option<Retained<NSString>> {
            self.parts().user.as_deref().map(NSString::from_str)
        }

        #[unsafe(method(setPercentEncodedUser:))]
        fn set_percent_encoded_user(&self, user: Option<&NSString>) {
            self.parts().user = user.map(|s| s.to_string());
        }

        #[unsafe(method_id(percentEncodedPassword))]
        fn percent_encoded_password(&self) -> Option<Retained<NSString>> {
            self.parts().password.as_deref().map(NSString::from_str)
        }

        #[unsafe(method(setPercentEncodedPassword:))]
        fn set_percent_encoded_password(&self, password: Option<&NSString>) {
            self.parts().password = password.map(|s| s.to_string());
        }

        #[unsafe(method_id(percentEncodedHost))]
        fn percent_encoded_host(&self) -> Option<Retained<NSString>> {
            self.parts().host.as_deref().map(NSString::from_str)
        }

        #[unsafe(method(setPercentEncodedHost:))]
        fn set_percent_encoded_host(&self, host: Option<&NSString>) {
            self.parts().host = host.map(|s| s.to_string());
        }

        #[unsafe(method_id(percentEncodedPath))]
        fn percent_encoded_path(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.parts().path))
        }

        #[unsafe(method(setPercentEncodedPath:))]
        fn set_percent_encoded_path(&self, path: Option<&NSString>) {
            self.parts().path = path.map(|s| s.to_string()).unwrap_or_default();
        }

        #[unsafe(method_id(percentEncodedQuery))]
        fn percent_encoded_query(&self) -> Option<Retained<NSString>> {
            self.parts().query.as_deref().map(NSString::from_str)
        }

        #[unsafe(method(setPercentEncodedQuery:))]
        fn set_percent_encoded_query(&self, query: Option<&NSString>) {
            self.parts().query = query.map(|s| s.to_string());
        }

        #[unsafe(method_id(percentEncodedFragment))]
        fn percent_encoded_fragment(&self) -> Option<Retained<NSString>> {
            self.parts().fragment.as_deref().map(NSString::from_str)
        }

        #[unsafe(method(setPercentEncodedFragment:))]
        fn set_percent_encoded_fragment(&self, fragment: Option<&NSString>) {
            self.parts().fragment = fragment.map(|s| s.to_string());
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(queryItems))]
        fn query_items(&self) -> Option<Retained<AnyObject>> {
            self.items(true)
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(percentEncodedQueryItems))]
        fn percent_encoded_query_items(&self) -> Option<Retained<AnyObject>> {
            self.items(false)
        }

        #[unsafe(method(setQueryItems:))]
        fn set_query_items(&self, items: Option<&AnyObject>) {
            self.parts().query = items.map(|items| join_items(items, true));
        }

        #[unsafe(method(setPercentEncodedQueryItems:))]
        fn set_percent_encoded_query_items(&self, items: Option<&AnyObject>) {
            self.parts().query = items.map(|items| join_items(items, false));
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSURLComponents>()).is_some_and(|other| {
                let other = components_impl(other);
                std::ptr::eq(self, other) || *self.parts() == other.parts().clone()
            })
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            crate::string::hash_str(&self.parts().string().unwrap_or_default())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let parts = self.parts().clone();
            let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(null)".to_string());
            let mut out = format!("<NSURLComponents {:p}> {{", self);
            let _ = write!(
                out,
                "scheme = {}, user = {}, password = {}, host = {}, port = {}, path = {}, query = {}, fragment = {}}}",
                show(&parts.scheme),
                show(&parts.user),
                show(&parts.password),
                show(&parts.host),
                show(&parts.port),
                parts.path,
                show(&parts.query),
                show(&parts.fragment),
            );
            NSString::from_str(&out)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            let parts = self.parts().clone();
            init_with(Self::alloc(), parts)
        }
    }

    unsafe impl NSObjectProtocol for NSURLComponentsImpl {}
);

#[cfg(feature = "collections")]
impl NSURLComponentsImpl {
    fn items(&self, decode: bool) -> Option<Retained<AnyObject>> {
        let query = self.parts().query.clone()?;
        let items: Vec<Retained<NSURLQueryItem>> =
            split_items(&query, decode).into_iter().map(|(name, value)| item(&name, value.as_deref())).collect();
        Some(objc2_foundation::NSArray::from_retained_slice(&items).into())
    }
}

impl NSURLComponentsImpl {
    fn parts(&self) -> MutexGuard<'_, Parts> {
        crate::thread::lock(&self.ivars().parts)
    }

    fn url_with_base(&self, base: Option<&NSURL>) -> Option<Retained<NSURL>> {
        let string = self.parts().string()?;
        super::make(string, base).map(super::as_url)
    }
}

fn components_impl(components: &NSURLComponents) -> &NSURLComponentsImpl {
    // SAFETY: every NSURLComponents is an instance of this class.
    unsafe { &*(components as *const NSURLComponents).cast::<NSURLComponentsImpl>() }
}

fn init_with(this: Allocated<NSURLComponentsImpl>, parts: Parts) -> Retained<NSURLComponentsImpl> {
    let this = this.set_ivars(ComponentsIvars { parts: Mutex::new(parts) });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn init_parsed(
    this: Allocated<NSURLComponentsImpl>,
    string: &str,
    encode: bool,
) -> Option<Retained<NSURLComponentsImpl>> {
    match super::prepare(string, encode).as_deref().and_then(Parts::parse) {
        Some(parts) => Some(init_with(this, parts)),
        None => {
            drop(this);
            None
        }
    }
}

fn url_string(url: &NSURL, resolve: bool) -> String {
    let url = super::url_impl(url);
    if resolve { url.absolute().0.to_string() } else { url.ivars().string.to_string() }
}

fn decoded(text: Option<&str>) -> Option<Retained<NSString>> {
    let text = text?;
    Some(NSString::from_str(&parse::decode(text).unwrap_or_else(|| text.to_string())))
}

/// Decoded, or `None` if the escapes don't make UTF-8.
fn decoded_strict(text: Option<&str>) -> Option<Retained<NSString>> {
    parse::decode(text?).map(|t| NSString::from_str(&t))
}

fn encoded(text: Option<&NSString>, keep: &str) -> Option<String> {
    text.map(|t| parse::encode_component(&t.to_string(), keep))
}

/// A path decoded, except for escaped slashes, which would change its
/// meaning; empty if it doesn't decode to UTF-8.
fn decode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for (i, piece) in path.split("%2F").enumerate() {
        if i > 0 {
            out.push_str("%2F");
        }
        match parse::decode(piece) {
            Some(piece) => out.push_str(&piece),
            None => return String::new(),
        }
    }
    out
}

fn encode_host(host: &str) -> String {
    if host.starts_with('[') && host.ends_with(']') {
        let inner = &host[1..host.len() - 1];
        format!("[{}]", parse::encode_component(inner, ":"))
    } else {
        parse::encode_component(host, HOST_KEEP)
    }
}

/// Split a query into names and values, decoded or not. An item without
/// `=` has no value; an empty query has no items.
#[cfg(any(test, feature = "collections"))]
fn split_items(query: &str, decode: bool) -> Vec<(String, Option<String>)> {
    if query.is_empty() {
        return Vec::new();
    }
    let text = |s: &str| if decode { parse::decode(s) } else { Some(s.to_string()) };
    query
        .split('&')
        .map(|item| match item.split_once('=') {
            Some((name, value)) => (text(name).unwrap_or_default(), text(value)),
            None => (text(item).unwrap_or_default(), None),
        })
        .collect()
}

/// A query from an array of query items, their names and values encoded
/// or taken as they are.
fn join_items(items: &AnyObject, encode: bool) -> String {
    // SAFETY: the caller passes an array of NSURLQueryItem.
    let count: NSUInteger = unsafe { msg_send![items, count] };
    let mut out = String::new();
    for i in 0..count {
        // SAFETY: `i` is in bounds.
        let item: Retained<AnyObject> = unsafe { msg_send![items, objectAtIndex: i] };
        let Some(item) = item.downcast_ref::<NSURLQueryItem>() else { continue };
        let item = query_item_impl(item);
        if i > 0 {
            out.push('&');
        }
        let text = |s: &str| if encode { parse::encode_component(s, ITEM_KEEP) } else { s.to_string() };
        out.push_str(&text(&item.ivars().name));
        if let Some(value) = &item.ivars().value {
            out.push('=');
            out.push_str(&text(value));
        }
    }
    out
}

pub(crate) struct QueryItemIvars {
    name: String,
    value: Option<String>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLQueryItem"]
    #[ivars = QueryItemIvars]
    pub(crate) struct NSURLQueryItemImpl;

    impl NSURLQueryItemImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_item(this, String::new(), None)
        }

        #[unsafe(method_id(initWithName:value:))]
        fn init_with_name(this: Allocated<Self>, name: &NSString, value: Option<&NSString>) -> Retained<Self> {
            init_item(this, name.to_string(), value.map(|v| v.to_string()))
        }

        #[unsafe(method_id(queryItemWithName:value:))]
        fn with_name(name: &NSString, value: Option<&NSString>) -> Retained<Self> {
            init_item(Self::alloc(), name.to_string(), value.map(|v| v.to_string()))
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().name)
        }

        #[unsafe(method_id(value))]
        fn value(&self) -> Option<Retained<NSString>> {
            self.ivars().value.as_deref().map(NSString::from_str)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSURLQueryItem>()).is_some_and(|o| {
                let o = query_item_impl(o);
                o.ivars().name == self.ivars().name && o.ivars().value == self.ivars().value
            })
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            let value = self.ivars().value.as_deref().map_or(0, crate::string::hash_str);
            crate::string::hash_str(&self.ivars().name) ^ value
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&format!(
                "<NSURLQueryItem {:p}> {{name = {}, value = {}}}",
                self,
                self.ivars().name,
                self.ivars().value.as_deref().unwrap_or("(null)")
            ))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            use objc2::Message;
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSURLQueryItemImpl {}
);

fn init_item(this: Allocated<NSURLQueryItemImpl>, name: String, value: Option<String>) -> Retained<NSURLQueryItemImpl> {
    let this = this.set_ivars(QueryItemIvars { name, value });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn query_item_impl(item: &NSURLQueryItem) -> &NSURLQueryItemImpl {
    // SAFETY: every NSURLQueryItem is an instance of this class.
    unsafe { &*(item as *const NSURLQueryItem).cast::<NSURLQueryItemImpl>() }
}

/// A new query item.
#[cfg(feature = "collections")]
fn item(name: &str, value: Option<&str>) -> Retained<NSURLQueryItem> {
    let name = NSString::from_str(name);
    let value = value.map(NSString::from_str);
    NSURLQueryItem::queryItemWithName_value(&name, value.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_items() {
        let items = split_items("&a&&=b&c=%20d+e", true);
        let expected: Vec<(String, Option<String>)> = vec![
            (String::new(), None),
            ("a".into(), None),
            (String::new(), None),
            (String::new(), Some("b".into())),
            ("c".into(), Some(" d+e".into())),
        ];
        assert_eq!(items, expected);
        assert!(split_items("", true).is_empty());
        assert_eq!(split_items("a=%FF", true), vec![("a".to_string(), None)]);
    }

    #[test]
    fn parts_round_trip() {
        for s in
            ["http://user:pw@host:8080/a%20b/c?x=1&y=&z#frag", "", "//host/path", "http://[::1]:80/", "mailto:x@y.com"]
        {
            assert_eq!(Parts::parse(s).unwrap().string().unwrap(), s);
        }
        let parts = Parts::parse("http://[::1]:80/").unwrap();
        assert_eq!(parts.host.as_deref(), Some("[::1]"));
        let parts = Parts { host: Some("h".into()), path: "p".into(), ..Parts::default() };
        assert_eq!(parts.string(), None);
        assert_eq!(decode_path("/a%2Fb%20c"), "/a%2Fb c");
    }
}

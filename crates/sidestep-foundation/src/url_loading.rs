//! URL requests, responses and completion-handler data tasks. Network I/O runs
//! off the caller's thread; completion handlers are serialized on the session's
//! operation queue. Custom NSURLProtocols use the same completion path.
use crate::thread::lock;
use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSOperationQueue, NSString, NSURL,
    NSURLProtocol, NSURLRequest, NSURLResponse, NSURLSessionConfiguration, NSURLSessionDataTask, NSURLSessionTask,
    NSZone,
};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Headers = BTreeMap<String, String>;
fn dictionary(headers: &Headers) -> Retained<NSDictionary<NSString, NSString>> {
    let keys: Vec<_> = headers.keys().map(|s| NSString::from_str(s)).collect();
    let values: Vec<_> = headers.values().map(|s| NSString::from_str(s)).collect();
    NSDictionary::from_retained_objects(&keys.iter().map(|s| &**s).collect::<Vec<_>>(), &values)
}
fn headers(dict: Option<&NSDictionary<NSString, NSString>>) -> Headers {
    dict.map(|d| d.keys().map(|k| (k.to_string(), d.objectForKey(&k).unwrap().to_string())).collect())
        .unwrap_or_default()
}
fn header(headers: &Headers, key: &str) -> Option<String> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.clone())
}
fn set_header(headers: &mut Headers, key: String, value: Option<String>) {
    headers.retain(|k, _| !k.eq_ignore_ascii_case(&key));
    if let Some(v) = value {
        headers.insert(key, v);
    }
}

#[derive(Clone)]
struct RequestState {
    url: Option<Retained<NSURL>>,
    method: String,
    headers: Headers,
    body: Option<Retained<NSData>>,
    cache: usize,
    timeout: f64,
    cookies: bool,
}
fn request_state(url: &NSURL, cache: usize, timeout: f64) -> RequestState {
    RequestState {
        url: Some(url.retain()),
        method: "GET".into(),
        headers: Headers::new(),
        body: None,
        cache,
        timeout,
        cookies: true,
    }
}
sidestep_runtime::static_class!(pub(crate) REQUEST, REQUEST_META = "NSURLRequest", || { let _ = Request::class(); });
sidestep_runtime::static_class!(pub(crate) MUTABLE_REQUEST, MUTABLE_REQUEST_META = "NSMutableURLRequest", || { let _ = MutableRequest::class(); });
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLRequest"]
    #[ivars = Mutex<RequestState>]
    struct Request;
    impl Request {
        #[unsafe(method_id(initWithURL:cachePolicy:timeoutInterval:))]
        fn init(this: Allocated<Self>, url: &NSURL, cache: usize, timeout: f64) -> Retained<Self> { let this = this.set_ivars(Mutex::new(request_state(url, cache, timeout))); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(initWithURL:))]
        fn init_url(this: Allocated<Self>, url: &NSURL) -> Retained<Self> { unsafe { msg_send![this, initWithURL: url, cachePolicy: 0usize, timeoutInterval: 60f64] } }
        #[unsafe(method_id(requestWithURL:))]
        fn request(url: &NSURL) -> Retained<NSURLRequest> { unsafe { msg_send![NSURLRequest::alloc(), initWithURL: url] } }
        #[unsafe(method_id(requestWithURL:cachePolicy:timeoutInterval:))]
        fn request_full(url: &NSURL, cache: usize, timeout: f64) -> Retained<NSURLRequest> { unsafe { msg_send![NSURLRequest::alloc(), initWithURL: url, cachePolicy: cache, timeoutInterval: timeout] } }
        #[unsafe(method_id(URL))]
        fn url(&self) -> Option<Retained<NSURL>> { lock(self.ivars()).url.clone() }
        #[unsafe(method_id(HTTPMethod))]
        fn method(&self) -> Retained<NSString> { NSString::from_str(&lock(self.ivars()).method) }
        #[unsafe(method_id(allHTTPHeaderFields))]
        fn headers(&self) -> Retained<NSDictionary<NSString, NSString>> { dictionary(&lock(self.ivars()).headers) }
        #[unsafe(method_id(valueForHTTPHeaderField:))]
        fn header(&self, field: &NSString) -> Option<Retained<NSString>> { header(&lock(self.ivars()).headers, &field.to_string()).map(|v| NSString::from_str(&v)) }
        #[unsafe(method_id(HTTPBody))]
        fn body(&self) -> Option<Retained<NSData>> { lock(self.ivars()).body.clone() }
        #[unsafe(method(cachePolicy))]
        fn cache(&self) -> usize { lock(self.ivars()).cache }
        #[unsafe(method(timeoutInterval))]
        fn timeout(&self) -> f64 { lock(self.ivars()).timeout }
        #[unsafe(method(HTTPShouldHandleCookies))]
        fn cookies(&self) -> bool { lock(self.ivars()).cookies }
        #[unsafe(method_id(copyWithZone:))]
        fn copy(&self, _zone: *mut NSZone) -> Retained<NSURLRequest> { copy_request(&lock(self.ivars()), false) }
        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy(&self, _zone: *mut NSZone) -> Retained<NSURLRequest> { copy_request(&lock(self.ivars()), true) }
    }
    unsafe impl NSObjectProtocol for Request {}
);
fn request_imp(request: &NSURLRequest) -> &Request {
    unsafe { &*(request as *const NSURLRequest).cast() }
}
fn copy_request(s: &RequestState, mutable: bool) -> Retained<NSURLRequest> {
    let class = if mutable { NSMutableURLRequest::class() } else { NSURLRequest::class() };
    let url = s.url.clone().unwrap_or_else(|| NSURL::URLWithString(&NSString::from_str("about:blank")).unwrap());
    let out: Retained<NSURLRequest> = unsafe { msg_send![class, requestWithURL: &*url] };
    *lock(request_imp(&out).ivars()) = s.clone();
    out
}
define_class!(
    #[unsafe(super(NSURLRequest, NSObject))]
    #[name = "NSMutableURLRequest"]
    struct MutableRequest;
    impl MutableRequest {
        #[unsafe(method_id(requestWithURL:))]
        fn request(url: &NSURL) -> Retained<Self> { unsafe { msg_send![Self::alloc(), initWithURL: url] } }
        #[unsafe(method_id(requestWithURL:cachePolicy:timeoutInterval:))]
        fn request_full(url: &NSURL, cache: usize, timeout: f64) -> Retained<Self> { unsafe { msg_send![Self::alloc(), initWithURL: url, cachePolicy: cache, timeoutInterval: timeout] } }
        #[unsafe(method(setURL:))]
        fn set_url(&self, url: Option<&NSURL>) { lock(request_imp(self).ivars()).url = url.map(|u| u.retain()); }
        #[unsafe(method(setHTTPMethod:))]
        fn set_method(&self, method: &NSString) { lock(request_imp(self).ivars()).method = method.to_string(); }
        #[unsafe(method(setValue:forHTTPHeaderField:))]
        fn set_header(&self, value: Option<&NSString>, field: &NSString) { set_header(&mut lock(request_imp(self).ivars()).headers, field.to_string(), value.map(ToString::to_string)); }
        #[unsafe(method(addValue:forHTTPHeaderField:))]
        fn add_header(&self, value: &NSString, field: &NSString) { let mut s = lock(request_imp(self).ivars()); let field = field.to_string(); let value = header(&s.headers, &field).map_or_else(|| value.to_string(), |v| format!("{v},{}", value)); set_header(&mut s.headers, field, Some(value)); }
        #[unsafe(method(setAllHTTPHeaderFields:))]
        fn set_headers(&self, fields: Option<&NSDictionary<NSString, NSString>>) { lock(request_imp(self).ivars()).headers = headers(fields); }
        #[unsafe(method(setHTTPBody:))]
        fn set_body(&self, body: Option<&NSData>) { lock(request_imp(self).ivars()).body = body.map(|b| b.retain()); }
        #[unsafe(method(setCachePolicy:))]
        fn set_cache(&self, cache: usize) { lock(request_imp(self).ivars()).cache = cache; }
        #[unsafe(method(setTimeoutInterval:))]
        fn set_timeout(&self, timeout: f64) { lock(request_imp(self).ivars()).timeout = timeout; }
        #[unsafe(method(setHTTPShouldHandleCookies:))]
        fn set_cookies(&self, cookies: bool) { lock(request_imp(self).ivars()).cookies = cookies; }
    }
    unsafe impl NSObjectProtocol for MutableRequest {}
);

#[derive(Clone)]
struct ResponseState {
    url: Retained<NSURL>,
    mime: Option<Retained<NSString>>,
    length: i64,
    encoding: Option<Retained<NSString>>,
}
sidestep_runtime::static_class!(pub(crate) RESPONSE, RESPONSE_META = "NSURLResponse", || { let _ = Response::class(); });
sidestep_runtime::static_class!(pub(crate) HTTP_RESPONSE, HTTP_RESPONSE_META = "NSHTTPURLResponse", || { let _ = HTTPResponse::class(); });
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLResponse"]
    #[ivars = ResponseState]
    struct Response;
    impl Response {
        #[unsafe(method_id(initWithURL:MIMEType:expectedContentLength:textEncodingName:))]
        fn init(this: Allocated<Self>, url: &NSURL, mime: Option<&NSString>, length: isize, encoding: Option<&NSString>) -> Retained<Self> {
            let this = this.set_ivars(ResponseState { url: url.retain(), mime: mime.map(|v| v.retain()), length: length as i64, encoding: encoding.map(|v| v.retain()) }); unsafe { msg_send![super(this), init] }
        }
        #[unsafe(method_id(URL))]
        fn url(&self) -> Retained<NSURL> { self.ivars().url.clone() }
        #[unsafe(method_id(MIMEType))]
        fn mime(&self) -> Option<Retained<NSString>> { self.ivars().mime.clone() }
        #[unsafe(method(expectedContentLength))]
        fn length(&self) -> i64 { self.ivars().length }
        #[unsafe(method_id(textEncodingName))]
        fn encoding(&self) -> Option<Retained<NSString>> { self.ivars().encoding.clone() }
        #[unsafe(method_id(suggestedFilename))]
        fn filename(&self) -> Retained<NSString> { self.ivars().url.lastPathComponent().filter(|s| !s.is_empty()).unwrap_or_else(|| NSString::from_str("Unknown")) }
        #[unsafe(method_id(copyWithZone:))]
        fn copy(&self, _zone: *mut NSZone) -> Retained<Self> { self.retain() }
    }
    unsafe impl NSObjectProtocol for Response {}
);
struct HTTPState {
    status: isize,
    headers: Headers,
}
define_class!(
    #[unsafe(super(NSURLResponse, NSObject))]
    #[name = "NSHTTPURLResponse"]
    #[ivars = HTTPState]
    struct HTTPResponse;
    impl HTTPResponse {
        #[unsafe(method_id(initWithURL:statusCode:HTTPVersion:headerFields:))]
        fn init(this: Allocated<Self>, url: &NSURL, status: isize, _version: Option<&NSString>, fields: Option<&NSDictionary<NSString, NSString>>) -> Retained<Self> {
            let headers = headers(fields);
            let length = header(&headers, "Content-Length").and_then(|s| s.parse::<isize>().ok()).unwrap_or(-1);
            let content_type = header(&headers, "Content-Type");
            let mime = content_type.as_ref().map(|t| NSString::from_str(t.split(';').next().unwrap().trim()));
            let encoding = content_type.as_ref().and_then(|t| t.split(';').skip(1).find_map(|p| p.trim().split_once('=').filter(|(k,_)| k.eq_ignore_ascii_case("charset")).map(|(_,v)| NSString::from_str(v.trim_matches('"')))));
            let this = this.set_ivars(HTTPState { status, headers });
            unsafe { msg_send![super(this), initWithURL: url, MIMEType: mime.as_deref(), expectedContentLength: length, textEncodingName: encoding.as_deref()] }
        }
        #[unsafe(method(statusCode))]
        fn status(&self) -> isize { self.ivars().status }
        #[unsafe(method_id(allHeaderFields))]
        fn headers(&self) -> Retained<NSDictionary<NSString, NSString>> { dictionary(&self.ivars().headers) }
        #[unsafe(method_id(valueForHTTPHeaderField:))]
        fn header(&self, field: &NSString) -> Option<Retained<NSString>> { header(&self.ivars().headers, &field.to_string()).map(|v| NSString::from_str(&v)) }
        #[unsafe(method_id(localizedStringForStatusCode:))]
        fn status_text(status: isize) -> Retained<NSString> { NSString::from_str(ureq::http::StatusCode::from_u16(status as u16).ok().and_then(|s| s.canonical_reason()).unwrap_or("Unknown")) }
    }
    unsafe impl NSObjectProtocol for HTTPResponse {}
);

crate::constant_string!(NSURLErrorDomain = "NSURLErrorDomain");
#[derive(Clone)]
struct ConfigState {
    protocols: Vec<usize>,
    timeout: f64,
    resource_timeout: f64,
    cache_policy: usize,
    cookie_policy: usize,
    cookies: bool,
    waits: bool,
    cache: Option<Retained<AnyObject>>,
}
impl Default for ConfigState {
    fn default() -> Self {
        Self {
            protocols: Vec::new(),
            timeout: 60.0,
            resource_timeout: 604800.0,
            cache_policy: 0,
            cookie_policy: 0,
            cookies: true,
            waits: false,
            cache: None,
        }
    }
}
sidestep_runtime::static_class!(pub(crate) CONFIG, CONFIG_META = "NSURLSessionConfiguration", || { let _ = Configuration::class(); });
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLSessionConfiguration"]
    #[ivars = Mutex<ConfigState>]
    struct Configuration;
    impl Configuration {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> { let this = this.set_ivars(Mutex::new(ConfigState::default())); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(defaultSessionConfiguration))]
        fn default_config() -> Retained<Self> { unsafe { msg_send![Self::alloc(), init] } }
        #[unsafe(method_id(ephemeralSessionConfiguration))]
        fn ephemeral() -> Retained<Self> { unsafe { msg_send![Self::alloc(), init] } }
        #[unsafe(method_id(copyWithZone:))]
        fn copy(&self, _zone: *mut NSZone) -> Retained<Self> { let this = Self::alloc().set_ivars(Mutex::new(lock(self.ivars()).clone())); unsafe { msg_send![super(this), init] } }
        #[unsafe(method(setProtocolClasses:))]
        fn set_protocols(&self, classes: Option<&NSArray<AnyClass>>) { lock(self.ivars()).protocols = classes.map(|a| a.iter().map(|c| Retained::as_ptr(&c) as usize).collect()).unwrap_or_default(); }
        #[unsafe(method_id(protocolClasses))]
        fn protocols(&self) -> Retained<NSArray<AnyClass>> { let s = lock(self.ivars()); let classes: Vec<&AnyClass> = s.protocols.iter().map(|p| unsafe { &*(*p as *const AnyClass) }).collect(); NSArray::from_slice(&classes) }
        #[unsafe(method_id(URLCache))]
        fn cache(&self) -> Option<Retained<AnyObject>> { lock(self.ivars()).cache.clone() }
        #[unsafe(method(setURLCache:))]
        fn set_cache(&self, value: Option<&AnyObject>) { lock(self.ivars()).cache = value.map(|v| v.retain()); }
        #[unsafe(method(timeoutIntervalForRequest))]
        fn timeout(&self) -> f64 { lock(self.ivars()).timeout }
        #[unsafe(method(setTimeoutIntervalForRequest:))]
        fn set_timeout(&self, value: f64) { lock(self.ivars()).timeout = value; }
        #[unsafe(method(timeoutIntervalForResource))]
        fn resource_timeout(&self) -> f64 { lock(self.ivars()).resource_timeout }
        #[unsafe(method(setTimeoutIntervalForResource:))]
        fn set_resource_timeout(&self, value: f64) { lock(self.ivars()).resource_timeout = value; }
        #[unsafe(method(requestCachePolicy))]
        fn cache_policy(&self) -> usize { lock(self.ivars()).cache_policy }
        #[unsafe(method(setRequestCachePolicy:))]
        fn set_cache_policy(&self, value: usize) { lock(self.ivars()).cache_policy = value; }
        #[unsafe(method(HTTPCookieAcceptPolicy))]
        fn cookie_policy(&self) -> usize { lock(self.ivars()).cookie_policy }
        #[unsafe(method(setHTTPCookieAcceptPolicy:))]
        fn set_cookie_policy(&self, value: usize) { lock(self.ivars()).cookie_policy = value; }
        #[unsafe(method(HTTPShouldSetCookies))]
        fn cookies(&self) -> bool { lock(self.ivars()).cookies }
        #[unsafe(method(setHTTPShouldSetCookies:))]
        fn set_cookies(&self, value: bool) { lock(self.ivars()).cookies = value; }
        #[unsafe(method(waitsForConnectivity))]
        fn waits(&self) -> bool { lock(self.ivars()).waits }
        #[unsafe(method(setWaitsForConnectivity:))]
        fn set_waits(&self, value: bool) { lock(self.ivars()).waits = value; }
    }
    unsafe impl NSObjectProtocol for Configuration {}
);
fn config_imp(c: &NSURLSessionConfiguration) -> &Configuration {
    unsafe { &*(c as *const NSURLSessionConfiguration).cast() }
}
type Completion = dyn Fn(*mut NSData, *mut NSURLResponse, *mut NSError);
struct SessionState {
    config: Retained<NSURLSessionConfiguration>,
    queue: Retained<NSOperationQueue>,
    tasks: Mutex<Vec<std::sync::Weak<TaskInner>>>,
    invalid: std::sync::atomic::AtomicBool,
}
sidestep_runtime::static_class!(pub(crate) SESSION, SESSION_META = "NSURLSession", || { let _ = Session::class(); });
sidestep_runtime::static_class!(pub(crate) TASK, TASK_META = "NSURLSessionTask", || { let _ = Task::class(); });
sidestep_runtime::static_class!(pub(crate) DATA_TASK, DATA_TASK_META = "NSURLSessionDataTask", || { let _ = DataTask::class(); });
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLSession"]
    #[ivars = SessionState]
    struct Session;
    impl Session {
        #[unsafe(method_id(sessionWithConfiguration:))]
        fn session(config: &NSURLSessionConfiguration) -> Retained<Self> { new_session(config, None) }
        #[unsafe(method_id(sessionWithConfiguration:delegate:delegateQueue:))]
        fn session_delegate(config: &NSURLSessionConfiguration, _delegate: Option<&AnyObject>, queue: Option<&NSOperationQueue>) -> Retained<Self> { new_session(config, queue) }
        #[unsafe(method_id(sharedSession))]
        fn shared() -> Retained<Self> { shared_session() }
        #[unsafe(method_id(configuration))]
        fn config(&self) -> Retained<NSURLSessionConfiguration> { unsafe { msg_send![&*self.ivars().config, copy] } }
        #[unsafe(method_id(delegateQueue))]
        fn queue(&self) -> Retained<NSOperationQueue> { self.ivars().queue.clone() }
        #[unsafe(method_id(dataTaskWithRequest:completionHandler:))]
        fn task(&self, request: &NSURLRequest, completion: &DynBlock<Completion>) -> Retained<NSURLSessionDataTask> { new_task(self, request, completion) }
        #[unsafe(method_id(dataTaskWithURL:completionHandler:))]
        fn task_url(&self, url: &NSURL, completion: &DynBlock<Completion>) -> Retained<NSURLSessionDataTask> { new_task(self, &NSURLRequest::requestWithURL(url), completion) }
        #[unsafe(method(invalidateAndCancel))]
        fn cancel(&self) { self.ivars().invalid.store(true, Ordering::Release); let tasks: Vec<_> = lock(&self.ivars().tasks).iter().filter_map(std::sync::Weak::upgrade).collect(); for task in tasks { cancel(&task); } }
        #[unsafe(method(finishTasksAndInvalidate))]
        fn finish(&self) { self.ivars().invalid.store(true, Ordering::Release); }
    }
    unsafe impl NSObjectProtocol for Session {}
);
fn new_session(config: &NSURLSessionConfiguration, queue: Option<&NSOperationQueue>) -> Retained<Session> {
    let config: Retained<NSURLSessionConfiguration> = unsafe { msg_send![config, copy] };
    let queue = queue.map(|q| q.retain()).unwrap_or_else(|| {
        let q = NSOperationQueue::new();
        q.setMaxConcurrentOperationCount(1);
        q
    });
    let this = Session::alloc().set_ivars(SessionState {
        config,
        queue,
        tasks: Mutex::new(Vec::new()),
        invalid: false.into(),
    });
    unsafe { msg_send![super(this), init] }
}
fn shared_session() -> Retained<Session> {
    static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = SHARED.get_or_init(|| {
        Retained::into_raw(new_session(&NSURLSessionConfiguration::defaultSessionConfiguration(), None)) as usize
    });
    unsafe { Retained::retain(*ptr as *mut Session).unwrap() }
}
struct TaskInner {
    request: Retained<NSURLRequest>,
    config: ConfigState,
    queue: Retained<NSOperationQueue>,
    id: usize,
    data: Mutex<TaskState>,
}
struct TaskState {
    state: isize,
    started: bool,
    completion: Option<RcBlock<Completion>>,
    response: Option<Retained<NSURLResponse>>,
    bytes: Vec<u8>,
    error: Option<Retained<NSError>>,
    protocol: Option<Retained<NSURLProtocol>>,
}
// NSURLSession data tasks own immutable request/configuration snapshots. Mutable
// state is locked; user callbacks are invoked only through the operation queue.
unsafe impl Send for TaskInner {}
unsafe impl Sync for TaskInner {}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLSessionTask"]
    #[ivars = Arc<TaskInner>]
    struct Task;
    impl Task {
        #[unsafe(method(resume))]
        fn resume(&self) {
            let task = self.ivars().clone();
            let start = { let mut s = lock(&task.data); if s.state == 3 || s.started { false } else { s.started = true; s.state = 0; true } };
            if start { let worker = task.clone(); if std::thread::Builder::new().name("NSURLSession".into()).spawn(move || objc2::rc::autoreleasepool(|_| load_task(worker))).is_err() { finish(&task, Some(url_error(-1))); } }
        }
        #[unsafe(method(cancel))]
        fn cancel(&self) { cancel(self.ivars()); }
        #[unsafe(method(state))]
        fn state(&self) -> isize { lock(&self.ivars().data).state }
        #[unsafe(method(taskIdentifier))]
        fn id(&self) -> usize { self.ivars().id }
        #[unsafe(method_id(originalRequest))]
        fn original(&self) -> Retained<NSURLRequest> { self.ivars().request.clone() }
        #[unsafe(method_id(currentRequest))]
        fn current(&self) -> Retained<NSURLRequest> { self.ivars().request.clone() }
        #[unsafe(method_id(response))]
        fn response(&self) -> Option<Retained<NSURLResponse>> { lock(&self.ivars().data).response.clone() }
        #[unsafe(method_id(error))]
        fn error(&self) -> Option<Retained<NSError>> { lock(&self.ivars().data).error.clone() }
        #[unsafe(method(countOfBytesReceived))]
        fn received(&self) -> i64 { lock(&self.ivars().data).bytes.len() as i64 }
        #[unsafe(method(countOfBytesExpectedToReceive))]
        fn expected(&self) -> i64 { lock(&self.ivars().data).response.as_ref().map_or(-1, |r| r.expectedContentLength()) }
    }
    unsafe impl NSObjectProtocol for Task {}
);
define_class!(
    #[unsafe(super(NSURLSessionTask, NSObject))]
    #[name = "NSURLSessionDataTask"]
    struct DataTask;
    unsafe impl NSObjectProtocol for DataTask {}
);
fn new_task(
    session: &Session,
    request: &NSURLRequest,
    completion: &DynBlock<Completion>,
) -> Retained<NSURLSessionDataTask> {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let request = copy_request(&lock(request_imp(request).ivars()), false);
    let task = Arc::new(TaskInner {
        request,
        config: lock(config_imp(&session.ivars().config).ivars()).clone(),
        queue: session.ivars().queue.clone(),
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        data: Mutex::new(TaskState {
            state: 1,
            started: false,
            completion: Some(completion.copy()),
            response: None,
            bytes: Vec::new(),
            error: None,
            protocol: None,
        }),
    });
    {
        let mut tasks = lock(&session.ivars().tasks);
        tasks.retain(|t| t.strong_count() != 0);
        tasks.push(Arc::downgrade(&task));
    }
    let this: Allocated<Task> = unsafe { msg_send![NSURLSessionDataTask::class(), alloc] };
    let this = this.set_ivars(task.clone());
    let this: Retained<Task> = unsafe { msg_send![super(this), init] };
    if session.ivars().invalid.load(Ordering::Acquire) {
        finish(&task, Some(url_error(-999)));
    }
    unsafe { Retained::cast_unchecked(this) }
}
fn url_error(code: isize) -> Retained<NSError> {
    unsafe { NSError::errorWithDomain_code_userInfo(&NSString::from_str("NSURLErrorDomain"), code, None) }
}
fn cancel(task: &Arc<TaskInner>) {
    let protocol = lock(&task.data).protocol.clone();
    finish(task, Some(url_error(-999)));
    if let Some(protocol) = protocol {
        protocol.stopLoading();
    }
}
fn finish(task: &Arc<TaskInner>, error: Option<Retained<NSError>>) {
    let (completion, protocol) = {
        let mut s = lock(&task.data);
        if s.state == 3 {
            return;
        }
        s.state = 3;
        s.error = error;
        (s.completion.take(), s.protocol.take())
    };
    drop(protocol);
    if let Some(completion) = completion {
        let task = task.clone();
        let queue = task.queue.clone();
        let block = RcBlock::new(move || {
            let (data, response, error) = {
                let s = lock(&task.data);
                (
                    if s.error.is_none() { Some(NSData::with_bytes(&s.bytes)) } else { None },
                    s.response.clone(),
                    s.error.clone(),
                )
            };
            completion.call((
                data.as_ref().map_or(std::ptr::null_mut(), |o| Retained::as_ptr(o).cast_mut()),
                response.as_ref().map_or(std::ptr::null_mut(), |o| Retained::as_ptr(o).cast_mut()),
                error.as_ref().map_or(std::ptr::null_mut(), |o| Retained::as_ptr(o).cast_mut()),
            ));
        });
        unsafe { queue.addOperationWithBlock(&block) };
    }
}
fn load_task(task: Arc<TaskInner>) {
    if lock(&task.data).state == 3 {
        return;
    }
    for ptr in &task.config.protocols {
        let class = unsafe { &*(*ptr as *const AnyClass) };
        let accepts: bool = unsafe { msg_send![class, canInitWithRequest: &*task.request] };
        if !accepts {
            continue;
        }
        let canonical: Retained<NSURLRequest> = unsafe { msg_send![class, canonicalRequestForRequest: &*task.request] };
        let client = Client::alloc().set_ivars(task.clone());
        let client: Retained<Client> = unsafe { msg_send![super(client), init] };
        let protocol: Allocated<NSURLProtocol> = unsafe { msg_send![class, alloc] };
        let protocol: Retained<NSURLProtocol> = unsafe {
            msg_send![protocol, initWithRequest: &*canonical, cachedResponse: None::<&AnyObject>, client: &*client]
        };
        {
            let mut s = lock(&task.data);
            if s.state == 3 {
                return;
            }
            s.protocol = Some(protocol.clone());
        }
        protocol.startLoading();
        return;
    }
    match http_load(&task) {
        Ok(()) => finish(&task, None),
        Err(code) => finish(&task, Some(url_error(code))),
    }
}
fn http_load(task: &Arc<TaskInner>) -> Result<(), isize> {
    let s = lock(request_imp(&task.request).ivars()).clone();
    let url = s.url.ok_or(-1000isize)?;
    let absolute = url.absoluteString().ok_or(-1000isize)?.to_string();
    if !absolute.starts_with("http://") && !absolute.starts_with("https://") {
        return Err(-1002);
    }
    let timeout = if s.timeout == 60.0 { task.config.timeout } else { s.timeout };
    let timeout = timeout.min(task.config.resource_timeout);
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(if timeout.is_finite() && timeout > 0.0 {
            Some(Duration::from_secs_f64(timeout))
        } else {
            None
        })
        .build();
    let agent = config.new_agent();
    let mut request = ureq::http::Request::builder().method(s.method.as_str()).uri(&absolute);
    for (key, value) in &s.headers {
        request = request.header(key, value);
    }
    let body = s.body.as_ref().map(|b| b.to_vec()).unwrap_or_default();
    let request = request.body(body.as_slice()).map_err(|_| -1000isize)?;
    let mut response = agent.run(request).map_err(|e| match e {
        ureq::Error::Timeout(_) => -1001isize,
        ureq::Error::HostNotFound => -1003,
        _ => -1004,
    })?;
    let status = response.status().as_u16() as isize;
    let headers: Headers =
        response.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string())).collect();
    let response_obj = NSHTTPURLResponse::initWithURL_statusCode_HTTPVersion_headerFields(
        NSHTTPURLResponse::alloc(),
        &url,
        status,
        Some(&NSString::from_str("HTTP/1.1")),
        Some(&dictionary(&headers)),
    )
    .ok_or(-1011isize)?;
    {
        let mut s = lock(&task.data);
        if s.state == 3 {
            return Ok(());
        }
        s.response = Some(Retained::into_super(response_obj));
    }
    let mut reader = response.body_mut().as_reader();
    let mut buffer = [0u8; 16384];
    loop {
        if lock(&task.data).state == 3 {
            return Ok(());
        }
        let count = reader
            .read(&mut buffer)
            .map_err(|e| if e.kind() == std::io::ErrorKind::TimedOut { -1001isize } else { -1005 })?;
        if count == 0 {
            break;
        }
        lock(&task.data).bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(())
}
struct ProtocolState {
    request: Retained<NSURLRequest>,
    client: Option<Retained<AnyObject>>,
    cached: Option<Retained<AnyObject>>,
}
sidestep_runtime::static_class!(pub(crate) PROTOCOL, PROTOCOL_META = "NSURLProtocol", || { let _ = Protocol::class(); });
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURLProtocol"]
    #[ivars = ProtocolState]
    struct Protocol;
    impl Protocol {
        #[unsafe(method_id(initWithRequest:cachedResponse:client:))]
        fn init(this: Allocated<Self>, request: &NSURLRequest, cached: Option<&AnyObject>, client: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(ProtocolState { request: copy_request(&lock(request_imp(request).ivars()), false), client: client.map(|c| c.retain()), cached: cached.map(|c| c.retain()) }); unsafe { msg_send![super(this), init] }
        }
        #[unsafe(method_id(request))]
        fn request(&self) -> Retained<NSURLRequest> { self.ivars().request.clone() }
        #[unsafe(method_id(client))]
        fn client(&self) -> Option<Retained<AnyObject>> { self.ivars().client.clone() }
        #[unsafe(method_id(cachedResponse))]
        fn cached(&self) -> Option<Retained<AnyObject>> { self.ivars().cached.clone() }
        #[unsafe(method(canInitWithRequest:))]
        fn can(_request: &NSURLRequest) -> bool { false }
        #[unsafe(method_id(canonicalRequestForRequest:))]
        fn canonical(request: &NSURLRequest) -> Retained<NSURLRequest> { request.retain() }
    }
    unsafe impl NSObjectProtocol for Protocol {}
);
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepURLProtocolClient"]
    #[ivars = Arc<TaskInner>]
    struct Client;
    impl Client {
        #[unsafe(method(URLProtocol:didReceiveResponse:cacheStoragePolicy:))]
        fn response(&self, _protocol: &NSURLProtocol, response: &NSURLResponse, _policy: usize) { let mut s = lock(&self.ivars().data); if s.state != 3 { s.response = Some(response.retain()); } }
        #[unsafe(method(URLProtocol:didLoadData:))]
        fn data(&self, _protocol: &NSURLProtocol, data: &NSData) { let mut s = lock(&self.ivars().data); if s.state != 3 { s.bytes.extend_from_slice(&data.to_vec()); } }
        #[unsafe(method(URLProtocolDidFinishLoading:))]
        fn finish(&self, _protocol: &NSURLProtocol) { finish(self.ivars(), None); }
        #[unsafe(method(URLProtocol:didFailWithError:))]
        fn fail(&self, _protocol: &NSURLProtocol, error: &NSError) { finish(self.ivars(), Some(error.retain())); }
    }
    unsafe impl NSObjectProtocol for Client {}
);

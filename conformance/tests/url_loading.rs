//! URLSession's public request/response/completion API, against a loopback server.
use block2::RcBlock;
use objc2::AnyThread;
use objc2_foundation::*;
use sidestep as _;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

fn server(delay: Duration) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut received = Vec::new();
        let mut b = [0; 1024];
        while !received.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut b).unwrap();
            assert!(n > 0);
            received.extend_from_slice(&b[..n]);
        }
        std::thread::sleep(delay);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: text/plain; charset=utf-8\r\nETag: fixture\r\nConnection: close\r\n\r\nhello");
        String::from_utf8(received).unwrap()
    });
    (format!("http://{address}/feed"), worker)
}
#[test]
fn task_snapshots_request_and_returns_http_response_off_caller_thread() {
    let (url, server) = server(Duration::ZERO);
    let url = NSURL::URLWithString(&NSString::from_str(&url)).unwrap();
    let request = NSMutableURLRequest::requestWithURL(&url);
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str("old")), &NSString::from_str("If-None-Match"));
    let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    config.setTimeoutIntervalForRequest(5.0);
    let session = NSURLSession::sessionWithConfiguration(&config);
    let caller = std::thread::current().id();
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
        let result = unsafe {
            assert!(error.is_null(), "{:?}", error.as_ref());
            let response = response.as_ref().unwrap().downcast_ref::<NSHTTPURLResponse>().unwrap();
            (
                data.as_ref().unwrap().to_vec(),
                response.statusCode(),
                response.valueForHTTPHeaderField(&NSString::from_str("eTaG")).unwrap().to_string(),
                std::thread::current().id(),
            )
        };
        tx.send(result).unwrap();
    });
    let task = unsafe { session.dataTaskWithRequest_completionHandler(&request, &block) };
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str("changed")), &NSString::from_str("If-None-Match"));
    assert_eq!(task.state(), NSURLSessionTaskState::Suspended);
    task.resume();
    task.resume();
    let (data, status, etag, callback_thread) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(data, b"hello");
    assert_eq!(status, 200);
    assert_eq!(etag, "fixture");
    assert_ne!(callback_thread, caller);
    assert_eq!(task.state(), NSURLSessionTaskState::Completed);
    let wire = server.join().unwrap().to_lowercase();
    assert!(wire.contains("if-none-match: old"), "{wire}");
    assert!(!wire.contains("changed"));
    assert!(rx.try_recv().is_err());
    session.finishTasksAndInvalidate();
}
#[test]
fn cancellation_completes_exactly_once_with_cancelled_error() {
    let session = NSURLSession::sessionWithConfiguration(&NSURLSessionConfiguration::ephemeralSessionConfiguration());
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |_data: *mut NSData, _response: *mut NSURLResponse, error: *mut NSError| {
        tx.send(unsafe { error.as_ref().unwrap().code() }).unwrap();
    });
    let task = unsafe {
        session.dataTaskWithURL_completionHandler(
            &NSURL::URLWithString(&NSString::from_str("https://example.invalid/")).unwrap(),
            &block,
        )
    };
    task.cancel();
    task.cancel();
    task.resume();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), -999);
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
}
#[test]
fn http_header_lookup_is_case_insensitive_and_response_metadata_is_parsed() {
    let url = NSURL::URLWithString(&NSString::from_str("https://example.invalid/feed")).unwrap();
    let headers = NSDictionary::from_slices(
        &[&*NSString::from_str("CONTENT-TYPE"), &*NSString::from_str("Content-Length")],
        &[&*NSString::from_str("text/plain; charset=utf-8"), &*NSString::from_str("42")],
    );
    let response = NSHTTPURLResponse::initWithURL_statusCode_HTTPVersion_headerFields(
        NSHTTPURLResponse::alloc(),
        &url,
        304,
        Some(&NSString::from_str("HTTP/1.1")),
        Some(&headers),
    )
    .unwrap();
    assert_eq!(response.statusCode(), 304);
    assert_eq!(response.expectedContentLength(), 42);
    assert_eq!(response.MIMEType().unwrap().to_string(), "text/plain");
    assert_eq!(response.textEncodingName().unwrap().to_string(), "utf-8");
    assert!(response.valueForHTTPHeaderField(&NSString::from_str("missing")).is_none());
}

#[test]
fn cancelling_a_running_request_completes_before_the_server_replies() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = [0; 1024];
        assert!(socket.read(&mut bytes).unwrap() > 0);
        accepted_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
    });
    let session = NSURLSession::sessionWithConfiguration(&NSURLSessionConfiguration::ephemeralSessionConfiguration());
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |data: *mut NSData, _response: *mut NSURLResponse, error: *mut NSError| {
        assert!(data.is_null());
        tx.send(unsafe { error.as_ref().unwrap().code() }).unwrap();
    });
    let url = NSURL::URLWithString(&NSString::from_str(&format!("http://{address}/delayed"))).unwrap();
    let task = unsafe { session.dataTaskWithURL_completionHandler(&url, &block) };
    task.resume();
    accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    task.cancel();
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), -999);
    release_tx.send(()).unwrap();
    server.join().unwrap();
    task.cancel();
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(task.state(), NSURLSessionTaskState::Completed);
}

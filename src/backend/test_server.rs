//! A tiny HTTP server for the backend tests.
//!
//! The backends fetch over HTTP with a blocking client, so a test server on
//! its own thread serves them canned responses without a node, a runtime or
//! another dependency. It also records what was asked for, which is how the
//! tests tell the fetch paths apart.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// What a backend asked for.
pub struct Request {
    pub path: String,
    /// The `Range` header, if the request had one.
    pub range: Option<String>,
}

/// What the server answers with.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(body: &str) -> Self {
        Response {
            status: 200,
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn status(status: u16) -> Self {
        Response {
            status,
            body: Vec::new(),
        }
    }

    pub fn bytes(status: u16, body: Vec<u8>) -> Self {
        Response { status, body }
    }
}

pub struct TestServer {
    /// The base URL to point a backend at.
    pub url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl TestServer {
    /// The paths the server was asked for, in order.
    pub fn paths(&self) -> Vec<String> {
        self.requests.lock().expect("the lock").clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts a server answering every request with `handler`. It stops once the
/// returned `TestServer` is dropped.
pub fn serve<H>(handler: H) -> TestServer
where
    H: Fn(&Request) -> Response + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let url = format!("http://{}", listener.local_addr().expect("the address"));
    listener.set_nonblocking(true).expect("non-blocking");

    let requests = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let thread_requests = requests.clone();
    let thread_stop = stop.clone();
    thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let mut reader = BufReader::new(stream);
                    let request = read_request(&mut reader);
                    thread_requests
                        .lock()
                        .expect("the lock")
                        .push(request.path.clone());
                    let response = handler(&request);
                    let mut stream = reader.into_inner();
                    let _ = write_response(&mut stream, &response);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(_) => break,
            }
        }
    });

    TestServer {
        url,
        requests,
        stop,
    }
}

fn read_request<R: BufRead>(reader: &mut R) -> Request {
    let mut request_line = String::new();
    reader.read_line(&mut request_line).expect("a request line");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    let mut range = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("a header line") == 0 || line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap_or(("", ""));
        if name.to_lowercase() == "range" {
            range = Some(value.trim().to_string());
        }
    }

    Request { path, range }
}

fn write_response<W: Write>(stream: &mut W, response: &Response) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

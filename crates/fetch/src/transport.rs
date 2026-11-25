//! HTTP behind a trait (the real client, and a fake for tests), and the clock, so the pacing and
//! waiting can be tested without waiting.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One honest identity, never varied.
pub const USER_AGENT: &str = "card-ledger/0.1 (personal collection tool)";

/// The largest body read (a TCGdex set listing is tens of KB; a JustTCG batch well under 1 MB).
const MAX_BODY: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// One HTTP request. Its `Debug` output never shows header values, so a key sent in a header
/// can't end up in a log or a test failure message.
#[derive(Clone, PartialEq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl Request {
    pub fn get(url: impl Into<String>) -> Request {
        Request {
            method: Method::Get,
            url: url.into(),
            headers: vec![("Accept".into(), "application/json".into())],
            body: None,
        }
    }

    pub fn post_json(url: impl Into<String>, body: String) -> Request {
        Request {
            method: Method::Post,
            url: url.into(),
            headers: vec![
                ("Accept".into(), "application/json".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            body: Some(body),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Request {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("header_names", &names)
            .field("body_len", &self.body.as_ref().map(String::len))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    /// A JSON answer (for fakes).
    pub fn json(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            )],
            body: body.to_string(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// No HTTP answer at all.
#[derive(Debug, Clone, PartialEq)]
pub enum NetError {
    Timeout(String),
    Failed(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Timeout(e) => write!(f, "timed out ({e})"),
            NetError::Failed(e) => write!(f, "{e}"),
        }
    }
}

pub trait Transport {
    /// `Err` means no HTTP answer (DNS, TLS, timeout). Any status code is `Ok`.
    fn send(&self, req: &Request) -> Result<Response, NetError>;
}

pub trait Clock {
    /// Unix milliseconds.
    fn now_ms(&self) -> u64;
    fn sleep_ms(&self, ms: u64);
    fn now_secs(&self) -> u64 {
        self.now_ms() / 1000
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// A clock that only moves when told to sleep (or `advance`d), and remembers each sleep.
#[derive(Debug, Default)]
pub struct FakeClock {
    now: Cell<u64>,
    pub sleeps: RefCell<Vec<u64>>,
}

impl FakeClock {
    /// A fake clock that starts at `ms` (Unix milliseconds).
    pub fn at(ms: u64) -> FakeClock {
        FakeClock {
            now: Cell::new(ms),
            sleeps: RefCell::new(Vec::new()),
        }
    }
    /// Moves time forward without counting it as a sleep (time passing between runs).
    pub fn advance_ms(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
    fn sleep_ms(&self, ms: u64) {
        self.sleeps.borrow_mut().push(ms);
        self.now.set(self.now.get() + ms);
    }
}

/// Answers from a queue, in order, and records every request. An empty queue answers with a
/// network failure, so a test that sends more than it expects fails loudly.
#[derive(Debug, Default)]
pub struct FakeTransport {
    replies: RefCell<VecDeque<Result<Response, NetError>>>,
    pub sent: RefCell<Vec<Request>>,
}

impl FakeTransport {
    pub fn new() -> FakeTransport {
        FakeTransport::default()
    }
    pub fn push(&self, r: Response) -> &Self {
        self.replies.borrow_mut().push_back(Ok(r));
        self
    }
    pub fn push_err(&self, e: NetError) -> &Self {
        self.replies.borrow_mut().push_back(Err(e));
        self
    }
    pub fn urls(&self) -> Vec<String> {
        self.sent.borrow().iter().map(|r| r.url.clone()).collect()
    }
    pub fn count(&self) -> usize {
        self.sent.borrow().len()
    }
}

impl Transport for FakeTransport {
    fn send(&self, req: &Request) -> Result<Response, NetError> {
        self.sent.borrow_mut().push(req.clone());
        self.replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err(NetError::Failed("fake transport: no reply queued".into())))
    }
}

/// HTTPS through Windows' own TLS and certificate store, with the one honest User-Agent: no
/// browser headers, no fingerprint tricks, no proxies. Redirects are not followed, so a request
/// only ever reaches the host it names. 10 s to connect, 30 s for the whole exchange.
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub fn new() -> Http {
        let agent = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .max_redirects(0)
            .https_only(true)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        Http { agent }
    }
}

impl Default for Http {
    fn default() -> Self {
        Http::new()
    }
}

fn net_err(e: ureq::Error) -> NetError {
    match e {
        ureq::Error::Timeout(t) => NetError::Timeout(t.to_string()),
        other => NetError::Failed(other.to_string()),
    }
}

impl Transport for Http {
    fn send(&self, req: &Request) -> Result<Response, NetError> {
        let result = match req.method {
            Method::Get => {
                let mut b = self.agent.get(&req.url);
                for (k, v) in &req.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                b.call()
            }
            Method::Post => {
                let mut b = self.agent.post(&req.url);
                for (k, v) in &req.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                b.send(req.body.as_deref().unwrap_or(""))
            }
        };
        let mut resp = result.map_err(net_err)?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .lossy_utf8(true)
            .read_to_string()
            .map_err(net_err)?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

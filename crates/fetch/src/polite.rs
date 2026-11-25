//! One polite client per source: paced, stops at a bot check, waits out one 429.

use crate::transport::{Clock, NetError, Request, Response, Transport};
use card_ledger_core::card::LookupError;
use card_ledger_core::time;
use std::collections::HashMap;
use std::fmt;

/// The least time between two requests to one host.
pub const MIN_GAP_MS: u64 = 500;
/// Pause before the one retry of a 502/503/504.
pub const TRANSIENT_RETRY_MS: u64 = 3_000;
/// The longest Retry-After we wait out (once per run).
pub const RETRY_CAP_S: u64 = 120;
/// Used when a 429 carries no usable Retry-After.
pub const DEFAULT_RETRY_S: u64 = 60;

/// Why a fetch failed or a run stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// No answer, a timeout or a 5xx: try again later. Nothing is concluded from it.
    Unavailable(String),
    /// The source asked us to slow down (429) and waiting once didn't help, or asked for longer
    /// than we wait. Stops the run.
    RateLimited(String),
    /// A bot check or challenge page (or HTML where JSON was expected). Stops the run; nothing
    /// here ever tries to get past one.
    Challenge(String),
    /// The answer could not be read.
    BadData(String),
    /// Another refusal: a 4xx such as 401 (bad key) or 403, or a redirect.
    Refused { status: u16, why: String },
    /// A local limit (JustTCG's free plan) would be passed by sending; nothing was sent.
    OverBudget(String),
    /// The local database failed.
    Storage(String),
}

impl FetchError {
    /// Errors after which a run should stop rather than go on to the next card.
    pub fn stops_run(&self) -> bool {
        matches!(
            self,
            FetchError::RateLimited(_)
                | FetchError::Challenge(_)
                | FetchError::OverBudget(_)
                | FetchError::Storage(_)
                | FetchError::Refused {
                    status: 401 | 403,
                    ..
                }
        )
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Unavailable(e) => write!(f, "source unavailable: {e}. Try again later"),
            FetchError::RateLimited(e) => write!(f, "the source asked card-ledger to slow down ({e}). Stopped; what was saved is kept. Try again later"),
            FetchError::Challenge(e) => write!(f, "the source answered with a bot check ({e}). Stopped; card-ledger never tries to get past one"),
            FetchError::BadData(e) => write!(f, "the source sent something unreadable: {e}"),
            FetchError::Refused { status, why } => write!(f, "the source refused the request ({status}): {why}"),
            FetchError::OverBudget(e) => write!(f, "not sent, it would pass the free-plan limit: {e}"),
            FetchError::Storage(e) => write!(f, "local database error: {e}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<FetchError> for LookupError {
    fn from(e: FetchError) -> LookupError {
        match e {
            FetchError::Unavailable(m) => LookupError::Unavailable(m),
            FetchError::RateLimited(m) => LookupError::RateLimited(m),
            FetchError::Challenge(m) => LookupError::RateLimited(format!(
                "bot check ({m}); stopped, card-ledger never tries to get past one"
            )),
            FetchError::BadData(m) => LookupError::BadData(m),
            // A 401/403 that isn't a bot check: the source refuses us, so asking again won't help.
            FetchError::Refused {
                status: status @ (401 | 403),
                why,
            } => LookupError::RateLimited(format!(
                "it refused access instead ({status}: {why}); stopped, try again later"
            )),
            e @ FetchError::Refused { .. } => LookupError::Unavailable(e.to_string()),
            FetchError::OverBudget(m) => LookupError::RateLimited(m),
            FetchError::Storage(m) => LookupError::Unavailable(format!("local database: {m}")),
        }
    }
}

/// A database error (core's `rusqlite::Error`) as a `FetchError`.
pub(crate) fn storage(e: impl fmt::Display) -> FetchError {
    FetchError::Storage(e.to_string())
}

/// A bot check or challenge page, by its headers or its HTML. Returns what gave it away.
pub fn detect_challenge(r: &Response) -> Option<String> {
    if r.header("cf-mitigated")
        .is_some_and(|v| v.to_ascii_lowercase().contains("challenge"))
    {
        return Some("cf-mitigated: challenge".into());
    }
    if r.header("x-datadome").is_some() {
        return Some("DataDome".into());
    }
    let head = r
        .body
        .get(..r.body.len().min(64 * 1024))
        .unwrap_or(&r.body)
        .to_ascii_lowercase();
    let markers: [(&str, &str); 9] = [
        (
            "<title>just a moment...</title>",
            "Cloudflare \"Just a moment...\" page",
        ),
        ("cf-browser-verification", "Cloudflare browser verification"),
        (
            "/cdn-cgi/challenge-platform/",
            "Cloudflare challenge platform",
        ),
        ("_cf_chl_opt", "Cloudflare challenge"),
        ("attention required! | cloudflare", "Cloudflare block page"),
        ("captcha-delivery.com", "DataDome captcha"),
        ("px-captcha", "PerimeterX captcha"),
        ("/_incapsula_resource", "Imperva/Incapsula check"),
        ("g-recaptcha", "reCAPTCHA"),
    ];
    if let Some((_, what)) = markers.iter().find(|(m, _)| head.contains(m)) {
        return Some((*what).to_string());
    }
    if r.status == 403 && looks_like_html(r) {
        return Some("403 with an HTML page".into());
    }
    None
}

/// True when the answer is an HTML page.
pub fn looks_like_html(r: &Response) -> bool {
    r.header("content-type")
        .is_some_and(|c| c.to_ascii_lowercase().contains("text/html"))
        || r.body.trim_start().starts_with('<')
}

/// Seconds to wait from a Retry-After value: a number of seconds or an HTTP date
/// ("Wed, 25 Nov 2026 01:48:47 GMT").
pub fn retry_after_secs(value: Option<&str>, now_secs: u64) -> Option<u64> {
    let v = value?.trim();
    if let Ok(n) = v.parse::<u64>() {
        return Some(n);
    }
    let parts: Vec<&str> = v.split_whitespace().collect();
    if parts.len() != 6 {
        return None;
    }
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let month = MONTHS
        .iter()
        .position(|m| parts[2].eq_ignore_ascii_case(m))?
        + 1;
    let day: u32 = parts[1].parse().ok()?;
    let iso = format!("{}-{month:02}-{day:02}T{}Z", parts[3], parts[4]);
    let at = time::unix_from_iso(&iso)?;
    Some(at.saturating_sub(now_secs))
}

/// The host of `https://host[:port]/...`, for pacing.
pub fn host_key(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// A short, single-line excerpt of a body for messages.
pub(crate) fn excerpt(body: &str) -> String {
    let one: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(160).collect()
}

/// The polite client. One request at a time (it is synchronous), at least `gap_ms` between two
/// requests to one host, a 429 waited out once per run (Retry-After up to 120 s), and a bot check
/// or a second 429 stops it for good: every later call returns the same error without sending.
pub struct Polite<'a> {
    t: &'a dyn Transport,
    clock: &'a dyn Clock,
    gap_ms: u64,
    last: HashMap<String, u64>,
    had_429: bool,
    stopped: Option<FetchError>,
    /// Requests actually sent.
    pub requests: u32,
}

impl<'a> Polite<'a> {
    pub fn new(t: &'a dyn Transport, clock: &'a dyn Clock, gap_ms: u64) -> Polite<'a> {
        Polite {
            t,
            clock,
            gap_ms: gap_ms.max(MIN_GAP_MS),
            last: HashMap::new(),
            had_429: false,
            stopped: None,
            requests: 0,
        }
    }

    pub fn clock(&self) -> &'a dyn Clock {
        self.clock
    }

    /// Why this client stopped, if it did.
    pub fn stopped(&self) -> Option<&FetchError> {
        self.stopped.as_ref()
    }

    fn stop(&mut self, e: FetchError) -> FetchError {
        self.stopped = Some(e.clone());
        e
    }

    fn once(
        &mut self,
        req: &Request,
        before_send: &mut dyn FnMut() -> Result<(), FetchError>,
    ) -> Result<Response, FetchError> {
        let host = host_key(&req.url);
        if let Some(last) = self.last.get(&host).copied() {
            let now = self.clock.now_ms();
            if now < last + self.gap_ms {
                self.clock.sleep_ms(last + self.gap_ms - now);
            }
        }
        if let Err(e) = before_send() {
            return Err(if e.stops_run() { self.stop(e) } else { e });
        }
        self.requests += 1;
        let r = self.t.send(req);
        self.last.insert(host, self.clock.now_ms());
        r.map_err(|e| match e {
            NetError::Timeout(m) => FetchError::Unavailable(format!("timed out: {m}")),
            NetError::Failed(m) => FetchError::Unavailable(m),
        })
    }

    /// One polite request. `before_send` runs right before each send (a retry included) and can
    /// refuse it; `retry_429` says whether a 429 may be waited out (false for "quota used up").
    /// 2xx answers that should be JSON but are HTML count as a challenge. 5xx is `Unavailable`.
    /// Other statuses come back as they are, for the caller to read.
    pub fn send(
        &mut self,
        req: &Request,
        expect_json: bool,
        before_send: &mut dyn FnMut() -> Result<(), FetchError>,
        retry_429: &dyn Fn(&Response) -> bool,
    ) -> Result<Response, FetchError> {
        if let Some(e) = &self.stopped {
            return Err(e.clone());
        }
        let mut r = self.once(req, before_send)?;
        if let Some(why) = detect_challenge(&r) {
            return Err(self.stop(FetchError::Challenge(format!(
                "{} at {}",
                why,
                host_key(&req.url)
            ))));
        }
        // At most one retry per request: after waiting out a 429, a 5xx is not retried again.
        let mut retried = false;
        if r.status == 429 {
            retried = true;
            let wait = retry_after_secs(r.header("retry-after"), self.clock.now_secs())
                .unwrap_or(DEFAULT_RETRY_S);
            if !retry_429(&r) {
                return Err(self.stop(FetchError::RateLimited(format!(
                    "429: {}",
                    excerpt(&r.body)
                ))));
            }
            if self.had_429 {
                return Err(self.stop(FetchError::RateLimited(
                    "a second 429 Too Many Requests in this run".into(),
                )));
            }
            self.had_429 = true;
            if wait > RETRY_CAP_S {
                return Err(self.stop(FetchError::RateLimited(format!("429, and Retry-After asks for {wait} s, more than the {RETRY_CAP_S} s card-ledger waits"))));
            }
            self.clock.sleep_ms(wait * 1000);
            r = self.once(req, before_send)?;
            if let Some(why) = detect_challenge(&r) {
                return Err(self.stop(FetchError::Challenge(format!(
                    "{} at {}",
                    why,
                    host_key(&req.url)
                ))));
            }
            if r.status == 429 {
                return Err(self.stop(FetchError::RateLimited(
                    "429 Too Many Requests again after waiting".into(),
                )));
            }
        }
        // A passing "no server available" (TCGdex answers 503 now and then) is retried once,
        // after a pause; a second 5xx is reported as unavailable. A Retry-After longer than the
        // pause means the source is down for a while: report it without retrying.
        if !retried && matches!(r.status, 502..=504) {
            let asked = retry_after_secs(r.header("retry-after"), self.clock.now_secs());
            if let Some(s) = asked.filter(|s| s.saturating_mul(1000) > TRANSIENT_RETRY_MS) {
                return Err(FetchError::Unavailable(format!(
                    "{} answered {} and asks to wait {s} s (Retry-After): {}",
                    host_key(&req.url),
                    r.status,
                    excerpt(&r.body)
                )));
            }
            self.clock
                .sleep_ms(asked.map_or(0, |s| s * 1000).max(TRANSIENT_RETRY_MS));
            r = self.once(req, before_send)?;
            if let Some(why) = detect_challenge(&r) {
                return Err(self.stop(FetchError::Challenge(format!(
                    "{} at {}",
                    why,
                    host_key(&req.url)
                ))));
            }
            if r.status == 429 {
                return Err(self.stop(FetchError::RateLimited(
                    "429 Too Many Requests after a retry".into(),
                )));
            }
        }
        if r.status >= 500 {
            return Err(FetchError::Unavailable(format!(
                "{} answered {}: {}",
                host_key(&req.url),
                r.status,
                excerpt(&r.body)
            )));
        }
        if expect_json && (200..300).contains(&r.status) && looks_like_html(&r) {
            return Err(self.stop(FetchError::Challenge(format!(
                "an HTML page where JSON was expected at {}",
                host_key(&req.url)
            ))));
        }
        Ok(r)
    }

    /// A plain GET with no hooks.
    pub fn get_json(&mut self, url: &str) -> Result<Response, FetchError> {
        self.send(&Request::get(url), true, &mut || Ok(()), &|_| true)
    }
}

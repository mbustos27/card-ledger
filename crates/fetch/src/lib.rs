//! The only network code in card-ledger: TCGdex (card identity and NM prices) and, behind a key,
//! JustTCG (per-condition prices).
//!
//! Manners, all enforced here:
//! - One honest User-Agent, `card-ledger/0.1 (personal collection tool)`; no browser headers,
//!   no fingerprint tricks, no proxies, no redirects followed.
//! - One request at a time (everything is synchronous), at least 500 ms between two requests to
//!   one host (6 s for JustTCG, its free plan allows 10 a minute). 10 s connect timeout, 30 s
//!   for a whole request.
//! - A 429 is honoured once per run: wait its Retry-After (at most 120 s; 60 s when it gives
//!   none), try once more. A second 429, or a longer Retry-After, stops the run.
//! - A 5xx or a timeout is "unavailable": nothing is concluded from it, and a price refresh stops
//!   after three in a row.
//! - A bot check or challenge page (Cloudflare and the like, or HTML where JSON was expected)
//!   stops the run at once. Nothing here ever tries to get around one.
//! - Answers are cached in the ledger database (cards 24 h, sets 30 days, 404s 7 days), and
//!   prices are only fetched again when they are older than the caller's limit.

pub mod justtcg;
pub mod polite;
pub mod refresh;
pub mod tcgdex;
pub mod transport;

pub use justtcg::{BatchProgress, ConditionReport, JustTcg, Quota};
pub use polite::{FetchError, Polite};
pub use refresh::{refresh_prices, Progress, RefreshReport, Step};
pub use tcgdex::Tcgdex;
pub use transport::{
    Clock, FakeClock, FakeTransport, Http, NetError, Request, Response, SystemClock, Transport,
    USER_AGENT,
};

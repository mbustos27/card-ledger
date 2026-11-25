//! JustTCG (`https://api.justtcg.com/v1/cards`): optional per-condition prices, free plan.
//!
//! - Off unless there is a key: `CARD_LEDGER_JUSTTCG_KEY` first, then the one saved with
//!   `card-ledger justtcg-key set` (see [`keystore`]). With no key nothing is ever sent. The key
//!   goes only into the `x-api-key` header of requests to `https://api.justtcg.com` and is never
//!   printed, logged or put in an error; it is only ever shown as its last four characters.
//! - Cards are matched by TCGplayer product id (from TCGdex's TCGplayer quotes), in batches of
//!   20 (`POST /v1/cards` with `[{"tcgplayerId": "…"}, …]`).
//! - Free-plan limits are kept locally and a request that would pass one is refused before it is
//!   sent: 100 a day (resets 00:00 UTC), 1,000 a month, 10 a minute (6 s between requests).
//!   The month is counted over the last 31 UTC days, since the free plan's month resets on the
//!   day the account was made, which we don't know; any billing month lies inside that window.
//!   Settings: "justtcg.day" = "YYYY-MM-DD <count>", "justtcg.month" = "YYYY-MM-DD:<count>,…".
//! - A 429 for the daily or monthly quota (DAILY_LIMIT_EXCEEDED / REQUEST_LIMIT_EXCEEDED) stops
//!   at once and marks the local count as used up; a per-minute 429 is waited out once.

use crate::polite::{excerpt, storage, FetchError, Polite};
use crate::transport::{Clock, Request, Response, Transport};
use card_ledger_core::card::{ConditionPrices, Lang};
use card_ledger_core::db::Db;
use card_ledger_core::model::Variant;
use card_ledger_core::time;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[path = "keystore.rs"]
pub mod keystore;

use keystore::KeyStore;

pub const ENV_KEY: &str = "CARD_LEDGER_JUSTTCG_KEY";
pub const URL: &str = "https://api.justtcg.com/v1/cards";
pub const SOURCE: &str = "justtcg";
pub const BATCH: usize = 20;
pub const DAY_LIMIT: u32 = 100;
pub const MONTH_LIMIT: u32 = 1000;
/// 10 requests a minute.
pub const GAP_MS: u64 = 6_000;
pub const MONTH_WINDOW_DAYS: u64 = 31;
pub const SETTING_DAY: &str = "justtcg.day";
pub const SETTING_MONTH: &str = "justtcg.month";

/// The API key. Its `Debug` and `Display` never show it.
#[derive(Clone)]
pub struct ApiKey(String);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(hidden)")
    }
}

/// Where the key in use came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// `CARD_LEDGER_JUSTTCG_KEY`.
    Env,
    /// The saved key; the store's [`KeyStore::location`].
    Stored(String),
}

/// What can be said about the key without showing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStatus {
    /// `None`: no key, condition prices are off.
    pub source: Option<KeySource>,
    /// The key's last four characters (`None` when unset or too short to show any).
    pub ends: Option<String>,
    /// Where `justtcg-key set` saves (and `clear` removes) the key.
    pub store: String,
    /// The env var is set and a saved key exists too (the env var wins).
    pub stored_too: bool,
}

impl KeyStatus {
    pub fn is_set(&self) -> bool {
        self.source.is_some()
    }

    /// "not set", "set (ends …abcd)" or "set".
    pub fn summary(&self) -> String {
        match (&self.source, &self.ends) {
            (None, _) => "not set".into(),
            (Some(_), Some(e)) => format!("set (ends …{e})"),
            (Some(_), None) => "set".into(),
        }
    }
}

/// The key to use: the env value (blank counts as unset) first, then the store's.
pub fn find_key(
    env: Option<&str>,
    store: &dyn KeyStore,
) -> Result<Option<(ApiKey, KeySource)>, String> {
    if let Some(k) = env.map(str::trim).filter(|k| !k.is_empty()) {
        return Ok(Some((ApiKey(k.to_string()), KeySource::Env)));
    }
    Ok(store
        .get()?
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .map(|k| (ApiKey(k), KeySource::Stored(store.location()))))
}

/// The key's status, from the env value and the store, without the key itself.
pub fn key_status(env: Option<&str>, store: &dyn KeyStore) -> Result<KeyStatus, String> {
    let found = find_key(env, store)?;
    let stored_too = matches!(found, Some((_, KeySource::Env)))
        && store.get()?.is_some_and(|k| !k.trim().is_empty());
    Ok(match found {
        None => KeyStatus {
            source: None,
            ends: None,
            store: store.location(),
            stored_too: false,
        },
        Some((k, src)) => KeyStatus {
            source: Some(src),
            ends: keystore::mask(&k.0),
            store: store.location(),
            stored_too,
        },
    })
}

/// The env var's value, if set.
pub fn env_key() -> Option<String> {
    std::env::var(ENV_KEY).ok()
}

/// Requests used, as the local counters see them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub day_used: u32,
    pub day_limit: u32,
    /// Over the last 31 UTC days.
    pub month_used: u32,
    pub month_limit: u32,
}

fn day_of(secs: u64) -> String {
    time::iso_from_unix(secs)[..10].to_string()
}

fn day_number(day: &str) -> Option<u64> {
    time::unix_from_iso(&format!("{day}T00:00:00Z")).map(|s| s / 86_400)
}

fn load_month(db: &Db, now_secs: u64) -> Result<BTreeMap<String, u32>, FetchError> {
    let today = now_secs / 86_400;
    let raw = db
        .setting(SETTING_MONTH)
        .map_err(storage)?
        .unwrap_or_default();
    let mut m = BTreeMap::new();
    for part in raw.split(',') {
        let Some((d, n)) = part.trim().split_once(':') else {
            continue;
        };
        let (Some(dn), Ok(n)) = (day_number(d), n.parse::<u32>()) else {
            continue;
        };
        if dn <= today && today - dn < MONTH_WINDOW_DAYS {
            *m.entry(d.to_string()).or_insert(0) += n;
        }
    }
    Ok(m)
}

fn load_day(db: &Db, now_secs: u64) -> Result<u32, FetchError> {
    let today = day_of(now_secs);
    let raw = db
        .setting(SETTING_DAY)
        .map_err(storage)?
        .unwrap_or_default();
    Ok(match raw.split_once(' ') {
        Some((d, n)) if d == today => n.trim().parse().unwrap_or(DAY_LIMIT),
        _ => 0,
    })
}

/// The local counters right now.
pub fn quota(db: &Db, now_secs: u64) -> Result<Quota, FetchError> {
    Ok(Quota {
        day_used: load_day(db, now_secs)?,
        day_limit: DAY_LIMIT,
        month_used: load_month(db, now_secs)?.values().sum(),
        month_limit: MONTH_LIMIT,
    })
}

fn save(db: &Db, now_secs: u64, day: u32, month: &BTreeMap<String, u32>) -> Result<(), FetchError> {
    db.set_setting(SETTING_DAY, &format!("{} {day}", day_of(now_secs)))
        .map_err(storage)?;
    let s: Vec<String> = month.iter().map(|(d, n)| format!("{d}:{n}")).collect();
    db.set_setting(SETTING_MONTH, &s.join(",")).map_err(storage)
}

/// Counts one request, or refuses it (`OverBudget`) when it would pass a free-plan limit.
pub fn take_request(db: &Db, now_secs: u64) -> Result<(), FetchError> {
    let q = quota(db, now_secs)?;
    if q.day_used >= DAY_LIMIT {
        return Err(FetchError::OverBudget(format!(
            "{} of {DAY_LIMIT} JustTCG requests used today (resets 00:00 UTC)",
            q.day_used
        )));
    }
    if q.month_used >= MONTH_LIMIT {
        return Err(FetchError::OverBudget(format!(
            "{} of {MONTH_LIMIT} JustTCG requests used in the last {MONTH_WINDOW_DAYS} days",
            q.month_used
        )));
    }
    let mut month = load_month(db, now_secs)?;
    *month.entry(day_of(now_secs)).or_insert(0) += 1;
    save(db, now_secs, q.day_used + 1, &month)
}

/// JustTCG said a quota is used up: make the local count agree, so nothing more is sent.
fn mark_used_up(db: &Db, now_secs: u64, code: &str) -> Result<(), FetchError> {
    let q = quota(db, now_secs)?;
    let mut month = load_month(db, now_secs)?;
    if code == "REQUEST_LIMIT_EXCEEDED" {
        let missing = MONTH_LIMIT.saturating_sub(q.month_used);
        *month.entry(day_of(now_secs)).or_insert(0) += missing;
    }
    save(db, now_secs, DAY_LIMIT.max(q.day_used), &month)
}

/// The error code in a JustTCG error body (`{"error": "...", "code": "..."}`).
pub fn error_code(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("code")?
        .as_str()
        .map(str::to_string)
}

fn is_quota_code(code: &str) -> bool {
    matches!(code, "DAILY_LIMIT_EXCEEDED" | "REQUEST_LIMIT_EXCEEDED")
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ConditionReport {
    /// Distinct cards asked about.
    pub cards: usize,
    /// Cards with no TCGplayer product id in their saved quotes (refresh TCGdex prices first).
    pub no_product_id: usize,
    /// Cards skipped because their condition prices are newer than the age limit.
    pub skipped_fresh: usize,
    /// Product ids JustTCG didn't return.
    pub not_found: usize,
    /// Cards that got at least one printing's condition prices.
    pub cards_priced: usize,
    /// ConditionPrices rows saved.
    pub saved: usize,
    /// Printing names that don't match a card-ledger variant (not saved).
    pub unknown_printings: Vec<String>,
    pub requests: u32,
    pub stop: Option<FetchError>,
}

/// Progress after each batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchProgress {
    pub batch: usize,
    pub batches: usize,
}

pub struct JustTcg<'a> {
    key: ApiKey,
    net: Polite<'a>,
    url: String,
}

impl fmt::Debug for JustTcg<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JustTcg")
            .field("key", &self.key)
            .field("url", &self.url)
            .field("requests", &self.net.requests)
            .finish()
    }
}

impl<'a> JustTcg<'a> {
    /// The key from `CARD_LEDGER_JUSTTCG_KEY`, else from `store`. `Ok(None)` (and nothing is ever
    /// sent) when neither has one. An error names the store, never the key.
    pub fn from_config(
        store: &dyn KeyStore,
        transport: &'a dyn Transport,
        clock: &'a dyn Clock,
    ) -> Result<Option<JustTcg<'a>>, String> {
        JustTcg::from_parts(env_key().as_deref(), store, transport, clock)
    }

    /// [`JustTcg::from_config`] with the env value given, for tests.
    pub fn from_parts(
        env: Option<&str>,
        store: &dyn KeyStore,
        transport: &'a dyn Transport,
        clock: &'a dyn Clock,
    ) -> Result<Option<JustTcg<'a>>, String> {
        Ok(find_key(env, store)?.and_then(|(k, _)| JustTcg::with_key(&k.0, transport, clock)))
    }

    /// `None` for a blank key.
    pub fn with_key(
        key: &str,
        transport: &'a dyn Transport,
        clock: &'a dyn Clock,
    ) -> Option<JustTcg<'a>> {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        Some(JustTcg {
            key: ApiKey(key.to_string()),
            net: Polite::new(transport, clock, GAP_MS),
            url: URL.to_string(),
        })
    }

    pub fn requests(&self) -> u32 {
        self.net.requests
    }

    /// Looks up per-condition prices for `cards` by the TCGplayer product ids in their latest
    /// saved quotes, 20 per request, and saves one `ConditionPrices` row per printing, dated now.
    /// A card with any condition-price row under `max_age_hours` old is skipped (`skipped_fresh`).
    pub fn refresh_conditions(
        &mut self,
        db: &Db,
        cards: &[(Lang, String)],
        max_age_hours: u64,
        progress: &mut dyn FnMut(BatchProgress),
    ) -> ConditionReport {
        let mut report = ConditionReport::default();
        let now = self.net.clock().now_secs();
        let max_age_s = max_age_hours.saturating_mul(3600);
        // product id -> cards carrying it (one product can cover two printings of a card).
        let mut by_pid: BTreeMap<u64, Vec<(Lang, String)>> = BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        for (lang, id) in cards {
            if !seen.insert((*lang, id.as_str())) {
                continue;
            }
            report.cards += 1;
            match has_fresh_condition_prices(db, *lang, id, now, max_age_s) {
                Ok(true) => {
                    report.skipped_fresh += 1;
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    report.stop = Some(e);
                    return report;
                }
            }
            let quotes = match db.latest_quotes(*lang, id) {
                Ok(q) => q,
                Err(e) => {
                    report.stop = Some(storage(e));
                    return report;
                }
            };
            let mut pids: Vec<u64> = quotes
                .iter()
                .filter(|q| q.quote.source == "tcgplayer")
                .filter_map(|q| q.quote.product_id)
                .collect();
            pids.sort_unstable();
            pids.dedup();
            if pids.is_empty() {
                report.no_product_id += 1;
            }
            for p in pids {
                by_pid.entry(p).or_default().push((*lang, id.clone()));
            }
        }
        let pids: Vec<u64> = by_pid.keys().copied().collect();
        let batches = pids.len().div_ceil(BATCH);
        let mut priced: std::collections::HashSet<(Lang, String)> = Default::default();
        let start = self.net.requests;
        for (i, chunk) in pids.chunks(BATCH).enumerate() {
            match self.batch(db, chunk) {
                Ok(found) => {
                    for pid in chunk {
                        let Some(prices) = found.get(pid) else {
                            report.not_found += 1;
                            continue;
                        };
                        for p in &prices.priced {
                            for (lang, id) in &by_pid[pid] {
                                let at = time::iso_from_unix(self.net.clock().now_secs());
                                if let Err(e) = db.save_condition_prices(*lang, id, p, &at) {
                                    report.stop = Some(storage(e));
                                    report.requests = self.net.requests - start;
                                    return report;
                                }
                                report.saved += 1;
                                priced.insert((*lang, id.clone()));
                            }
                        }
                        for u in &prices.unknown {
                            if !report.unknown_printings.contains(u) {
                                report.unknown_printings.push(u.clone());
                            }
                        }
                    }
                }
                Err(e) => {
                    report.stop = Some(e);
                    break;
                }
            }
            progress(BatchProgress {
                batch: i + 1,
                batches,
            });
        }
        report.cards_priced = priced.len();
        report.requests = self.net.requests - start;
        report
    }

    /// One batch request: product id -> its prices.
    fn batch(&mut self, db: &Db, pids: &[u64]) -> Result<HashMap<u64, ParsedCard>, FetchError> {
        let items: Vec<Value> = pids
            .iter()
            .map(|p| serde_json::json!({ "tcgplayerId": p.to_string() }))
            .collect();
        let body = Value::Array(items).to_string();
        let req = Request::post_json(self.url.clone(), body).with_header("x-api-key", &self.key.0);
        let clock = self.net.clock();
        let quota_code: RefCell<Option<String>> = RefCell::new(None);
        let r = self.net.send(
            &req,
            true,
            &mut || take_request(db, clock.now_secs()),
            &|r: &Response| match error_code(&r.body) {
                Some(c) if is_quota_code(&c) => {
                    *quota_code.borrow_mut() = Some(c);
                    false
                }
                _ => true,
            },
        );
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                if let Some(code) = quota_code.borrow().as_deref() {
                    mark_used_up(db, clock.now_secs(), code)?;
                    return Err(FetchError::RateLimited(format!(
                        "JustTCG says the free-plan quota is used up ({code})"
                    )));
                }
                return Err(e);
            }
        };
        match r.status {
            200..=299 => parse_batch(&r.body),
            s => {
                let code = error_code(&r.body).unwrap_or_default();
                Err(FetchError::Refused {
                    status: s,
                    why: format!("JustTCG {code} {}", excerpt(&r.body))
                        .trim()
                        .to_string(),
                })
            }
        }
    }
}

/// True when any printing of the card has a condition-price row under `max_age_s` old.
fn has_fresh_condition_prices(
    db: &Db,
    lang: Lang,
    card_id: &str,
    now: u64,
    max_age_s: u64,
) -> Result<bool, FetchError> {
    for v in Variant::ALL {
        if let Some(row) = db
            .latest_condition_prices(lang, card_id, v)
            .map_err(storage)?
        {
            if time::unix_from_iso(&row.fetched_at)
                .is_some_and(|t| now.saturating_sub(t) < max_age_s)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// One JustTCG card, parsed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedCard {
    pub priced: Vec<ConditionPrices>,
    pub unknown: Vec<String>,
}

/// Which `ConditionPrices` field a JustTCG condition fills (Sealed and unknown ones: none).
fn condition_slot<'p>(p: &'p mut ConditionPrices, condition: &str) -> Option<&'p mut Option<f64>> {
    Some(match condition.trim().to_ascii_lowercase().as_str() {
        "near mint" | "nm" => &mut p.nm,
        "lightly played" | "lp" => &mut p.lp,
        "moderately played" | "mp" => &mut p.mp,
        "heavily played" | "hp" => &mut p.hp,
        "damaged" | "dmg" => &mut p.dmg,
        _ => return None,
    })
}

/// Parses a `/v1/cards` answer (`{"data": [card, …]}`) into prices per TCGplayer product id.
/// Variants are grouped by printing; a printing name that isn't one of ours is reported, not
/// guessed. Non-English variants are skipped.
pub fn parse_batch(json: &str) -> Result<HashMap<u64, ParsedCard>, FetchError> {
    let bad = |w: &str| FetchError::BadData(format!("JustTCG: {w}"));
    let v: Value = serde_json::from_str(json).map_err(|e| bad(&format!("JSON: {e}")))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no data list"))?;
    let mut out: HashMap<u64, ParsedCard> = HashMap::new();
    for card in data {
        let pid = match card.get("tcgplayerId") {
            Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
            Some(Value::Number(n)) => n.as_u64(),
            _ => None,
        };
        let Some(pid) = pid else { continue };
        let entry = out.entry(pid).or_default();
        let mut by_printing: BTreeMap<String, (ConditionPrices, u64)> = BTreeMap::new();
        for var in card
            .get("variants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if var
                .get("language")
                .and_then(Value::as_str)
                .is_some_and(|l| !l.eq_ignore_ascii_case("english"))
            {
                continue;
            }
            let printing = var
                .get("printing")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let Some(variant) = Variant::parse(&printing) else {
                if !entry.unknown.contains(&printing) {
                    entry.unknown.push(printing);
                }
                continue;
            };
            let (cp, newest) = by_printing
                .entry(variant.label().to_string())
                .or_insert_with(|| {
                    (
                        ConditionPrices {
                            source: SOURCE.into(),
                            variant,
                            nm: None,
                            lp: None,
                            mp: None,
                            hp: None,
                            dmg: None,
                            updated: None,
                        },
                        0,
                    )
                });
            let condition = var.get("condition").and_then(Value::as_str).unwrap_or("");
            let price = var
                .get("price")
                .and_then(Value::as_f64)
                .filter(|p| p.is_finite() && *p > 0.0);
            if let Some(slot) = condition_slot(cp, condition) {
                *slot = price;
                if let Some(t) = var
                    .get("lastUpdated")
                    .and_then(Value::as_u64)
                    .filter(|_| price.is_some())
                {
                    *newest = (*newest).max(t);
                }
            }
        }
        for (_, (mut cp, newest)) in by_printing {
            if [cp.nm, cp.lp, cp.mp, cp.hp, cp.dmg]
                .iter()
                .all(Option::is_none)
            {
                continue;
            }
            cp.updated = (newest > 0).then(|| time::iso_from_unix(newest));
            entry.priced.push(cp);
        }
    }
    Ok(out)
}

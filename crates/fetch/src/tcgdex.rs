//! TCGdex (`https://api.tcgdex.net/v2`): card identity, printings and TCGplayer/Cardmarket
//! prices, cached in the ledger database.
//!
//! Cache (`catalog_cache`, by kind): "card" 24 h (the JSON carries prices), "set" 30 days,
//! "card-missing" / "set-missing" 7 days (a 404 is remembered so it isn't asked again).

use crate::polite::{excerpt, storage, FetchError, Polite, MIN_GAP_MS};
use crate::transport::{Clock, Transport};
use card_ledger_core::card::{
    CardBrief, CardLookup, CardRecord, Lang, LookupError, MarketPrice, Printings,
};
use card_ledger_core::db::Db;
use card_ledger_core::model::Variant;
use card_ledger_core::time;
use serde_json::Value;

pub const BASE: &str = "https://api.tcgdex.net/v2";
pub const CARD_TTL_S: u64 = 24 * 3600;
pub const SET_TTL_S: u64 = 30 * 86_400;
pub const MISSING_TTL_S: u64 = 7 * 86_400;

pub const KIND_CARD: &str = "card";
pub const KIND_CARD_MISSING: &str = "card-missing";
pub const KIND_SET: &str = "set";
pub const KIND_SET_MISSING: &str = "set-missing";

#[derive(Clone, Copy)]
struct Kind {
    found: &'static str,
    missing: &'static str,
    path: &'static str,
    ttl_s: u64,
}

const CARD: Kind = Kind {
    found: KIND_CARD,
    missing: KIND_CARD_MISSING,
    path: "cards",
    ttl_s: CARD_TTL_S,
};
const SET: Kind = Kind {
    found: KIND_SET,
    missing: KIND_SET_MISSING,
    path: "sets",
    ttl_s: SET_TTL_S,
};

/// What the cache holds for one id.
enum Cached {
    Fresh(String),
    FreshMissing,
    Stale(String),
    Nothing,
}

/// The TCGdex client. Build it with the real [`crate::Http`] and [`crate::SystemClock`], or
/// fakes in tests:
/// `let http = Http::new(); let clock = SystemClock; let mut t = Tcgdex::new(&db, &http, &clock);`
pub struct Tcgdex<'a> {
    db: &'a Db,
    net: Polite<'a>,
    base: String,
}

impl<'a> Tcgdex<'a> {
    pub fn new(db: &'a Db, transport: &'a dyn Transport, clock: &'a dyn Clock) -> Tcgdex<'a> {
        Tcgdex {
            db,
            net: Polite::new(transport, clock, MIN_GAP_MS),
            base: BASE.to_string(),
        }
    }

    /// Requests sent so far.
    pub fn requests(&self) -> u32 {
        self.net.requests
    }

    /// Why this client stopped (a bot check, or rate limiting), if it did.
    pub fn stopped(&self) -> Option<&FetchError> {
        self.net.stopped()
    }

    pub(crate) fn now_secs(&self) -> u64 {
        self.net.clock().now_secs()
    }

    pub(crate) fn now_iso(&self) -> String {
        time::iso_from_unix(self.now_secs())
    }

    fn age_ok(&self, fetched_at: &str, ttl_s: u64) -> bool {
        time::unix_from_iso(fetched_at).is_some_and(|at| self.now_secs().saturating_sub(at) < ttl_s)
    }

    fn cached(&self, kind: Kind, lang: Lang, id: &str) -> Result<Cached, FetchError> {
        let found = self.db.cache_get(kind.found, lang, id).map_err(storage)?;
        let missing = self.db.cache_get(kind.missing, lang, id).map_err(storage)?;
        let found_at = found
            .as_ref()
            .and_then(|(_, at)| time::unix_from_iso(at))
            .unwrap_or(0);
        let missing_at = missing
            .as_ref()
            .and_then(|(_, at)| time::unix_from_iso(at))
            .unwrap_or(0);
        // The newer of the two answers wins.
        if let Some((_, at)) = missing
            .as_ref()
            .filter(|_| missing_at > found_at || found.is_none())
        {
            if self.age_ok(at, MISSING_TTL_S) {
                return Ok(Cached::FreshMissing);
            }
            return Ok(found.map_or(Cached::Nothing, |(json, _)| Cached::Stale(json)));
        }
        Ok(match found {
            Some((json, at)) if self.age_ok(&at, kind.ttl_s) => Cached::Fresh(json),
            Some((json, _)) => Cached::Stale(json),
            None => Cached::Nothing,
        })
    }

    /// Fetches from the network and caches the answer (a 404 as "missing"). `check` validates
    /// the JSON before it is cached.
    fn fetch<T>(
        &mut self,
        kind: Kind,
        lang: Lang,
        id: &str,
        check: impl Fn(&str) -> Result<T, FetchError>,
    ) -> Result<Option<T>, FetchError> {
        let url = format!(
            "{}/{}/{}/{}",
            self.base,
            lang.code(),
            kind.path,
            path_segment(id)
        );
        let r = self.net.get_json(&url)?;
        let now = self.now_iso();
        match r.status {
            200..=299 => {
                let v = check(&r.body)?;
                self.db
                    .cache_put(kind.found, lang, id, &r.body, &now)
                    .map_err(storage)?;
                Ok(Some(v))
            }
            404 => {
                self.db
                    .cache_put(kind.missing, lang, id, "{}", &now)
                    .map_err(storage)?;
                Ok(None)
            }
            s => Err(FetchError::Refused {
                status: s,
                why: format!("{url}: {}", excerpt(&r.body)),
            }),
        }
    }

    fn lookup<T>(
        &mut self,
        kind: Kind,
        lang: Lang,
        id: &str,
        parse: impl Fn(&str) -> Result<T, FetchError>,
    ) -> Result<Option<T>, FetchError> {
        if !valid_id(id) {
            return Ok(None);
        }
        let stale = match self.cached(kind, lang, id)? {
            Cached::Fresh(json) => match parse(&json) {
                Ok(v) => return Ok(Some(v)),
                Err(_) => None, // a cache entry we can't read: fetch it again
            },
            Cached::FreshMissing => return Ok(None),
            Cached::Stale(json) => Some(json),
            Cached::Nothing => None,
        };
        match self.fetch(kind, lang, id, &parse) {
            // The source is down: an older answer is better than none (identity barely changes).
            Err(FetchError::Unavailable(e)) => match stale.as_deref().map(&parse) {
                Some(Ok(v)) => Ok(Some(v)),
                _ => Err(FetchError::Unavailable(e)),
            },
            other => other,
        }
    }

    /// A card, from the cache when it is under 24 h old.
    pub fn card_cached(&mut self, lang: Lang, id: &str) -> Result<Option<CardRecord>, FetchError> {
        self.lookup(CARD, lang, id, |j| parse_card(lang, j))
    }

    /// A card straight from TCGdex, ignoring the 24 h card cache (the answer is cached again).
    /// A 404 remembered in the last 7 days is still honoured: `Ok(None)` without a request.
    pub fn card_fresh(&mut self, lang: Lang, id: &str) -> Result<Option<CardRecord>, FetchError> {
        if !valid_id(id) {
            return Ok(None);
        }
        if let Cached::FreshMissing = self.cached(CARD, lang, id)? {
            return Ok(None);
        }
        self.fetch(CARD, lang, id, |j| parse_card(lang, j))
    }

    /// The cached card JSON for `id` with when it was fetched, however old.
    pub(crate) fn cached_card(
        &self,
        lang: Lang,
        id: &str,
    ) -> Result<Option<(CardRecord, String)>, FetchError> {
        match self.db.cache_get(KIND_CARD, lang, id).map_err(storage)? {
            Some((json, at)) => Ok(parse_card(lang, &json).ok().map(|c| (c, at))),
            None => Ok(None),
        }
    }

    /// A set's card list, from the cache when it is under 30 days old.
    pub fn set_cards_cached(
        &mut self,
        lang: Lang,
        set_id: &str,
    ) -> Result<Option<Vec<CardBrief>>, FetchError> {
        self.lookup(SET, lang, set_id, parse_set_cards)
    }
}

impl CardLookup for Tcgdex<'_> {
    fn card(&mut self, lang: Lang, id: &str) -> Result<Option<CardRecord>, LookupError> {
        self.card_cached(lang, id).map_err(Into::into)
    }
    fn set_cards(
        &mut self,
        lang: Lang,
        set_id: &str,
    ) -> Result<Option<Vec<CardBrief>>, LookupError> {
        self.set_cards_cached(lang, set_id).map_err(Into::into)
    }
}

/// Ids are used as one URL path segment: empty or dots-only ids are never sent.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.chars().all(|c| c == '.')
}

/// Percent-encodes everything but unreserved characters, so an id can't add path segments or a
/// query.
pub fn path_segment(id: &str) -> String {
    let mut s = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn bad(what: &str) -> FetchError {
    FetchError::BadData(format!("TCGdex: {what}"))
}

fn str_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A price figure; zero and negative values mean "no figure".
fn price_of(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
        .filter(|p| p.is_finite() && *p > 0.0)
}

/// Parses a TCGdex card answer.
pub fn parse_card(lang: Lang, json: &str) -> Result<CardRecord, FetchError> {
    let v: Value = serde_json::from_str(json).map_err(|e| bad(&format!("card JSON: {e}")))?;
    let id = v
        .get("id")
        .and_then(str_of)
        .ok_or_else(|| bad("card without an id"))?;
    let name = v
        .get("name")
        .and_then(str_of)
        .ok_or_else(|| bad("card without a name"))?;
    let local_id = v.get("localId").and_then(str_of).unwrap_or_default();
    let set = v.get("set");
    let set_id = set
        .and_then(|s| s.get("id"))
        .and_then(str_of)
        .unwrap_or_default();
    let set_name = set
        .and_then(|s| s.get("name"))
        .and_then(str_of)
        .unwrap_or_default();
    let flag = |k: &str| {
        v.get("variants")
            .and_then(|p| p.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let printings = Printings {
        normal: flag("normal"),
        reverse: flag("reverse"),
        holo: flag("holo"),
        first_edition: flag("firstEdition"),
    };
    let dex_ids = v
        .get("dexId")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_u64)
                .filter_map(|n| u32::try_from(n).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(CardRecord {
        lang,
        id,
        set_id,
        set_name,
        local_id,
        name,
        rarity: v.get("rarity").and_then(str_of),
        category: v.get("category").and_then(str_of),
        dex_ids,
        printings,
        prices: parse_prices(v.get("pricing")),
        image: v.get("image").and_then(str_of),
    })
}

/// The printing a TCGplayer bucket stands for, only where it is certain (these are TCGplayer's
/// printing names, which Collectr uses too).
pub fn tcgplayer_bucket_variant(bucket: &str) -> Option<Variant> {
    Some(match bucket {
        "normal" => Variant::Normal,
        "holofoil" => Variant::Holofoil,
        "reverse-holofoil" => Variant::ReverseHolofoil,
        "1st-edition" | "1st-edition-normal" => Variant::FirstEdition,
        "1st-edition-holofoil" => Variant::FirstEditionHolofoil,
        "unlimited" | "unlimited-normal" => Variant::Unlimited,
        "unlimited-holofoil" => Variant::UnlimitedHolofoil,
        _ => return None,
    })
}

/// The `pricing` object: one line per TCGplayer bucket (every object-valued key), and up to two
/// Cardmarket lines ("trend" and "trend-holo", variant unknown).
pub fn parse_prices(pricing: Option<&Value>) -> Vec<MarketPrice> {
    let mut out = Vec::new();
    let Some(p) = pricing else { return out };
    if let Some(tp) = p.get("tcgplayer").and_then(Value::as_object) {
        let currency = tp
            .get("unit")
            .and_then(str_of)
            .unwrap_or_else(|| "USD".into());
        let updated = tp.get("updated").and_then(str_of);
        let mut keys: Vec<&String> = tp.keys().collect();
        keys.sort();
        for k in keys {
            let Some(b) = tp.get(k).and_then(Value::as_object) else {
                continue;
            };
            out.push(MarketPrice {
                source: "tcgplayer".into(),
                bucket: k.clone(),
                variant: tcgplayer_bucket_variant(k),
                market: price_of(b.get("marketPrice")),
                low: price_of(b.get("lowPrice")),
                currency: currency.clone(),
                updated: updated.clone(),
                product_id: b.get("productId").and_then(Value::as_u64),
            });
        }
    }
    if let Some(cm) = p.get("cardmarket").and_then(Value::as_object) {
        let currency = cm
            .get("unit")
            .and_then(str_of)
            .unwrap_or_else(|| "EUR".into());
        let updated = cm.get("updated").and_then(str_of);
        for (bucket, trend, low) in [
            ("trend", "trend", "low"),
            ("trend-holo", "trend-holo", "low-holo"),
        ] {
            let (market, low) = (price_of(cm.get(trend)), price_of(cm.get(low)));
            if market.is_none() && low.is_none() {
                continue;
            }
            out.push(MarketPrice {
                source: "cardmarket".into(),
                bucket: bucket.into(),
                variant: None,
                market,
                low,
                currency: currency.clone(),
                updated: updated.clone(),
                product_id: None,
            });
        }
    }
    out
}

/// Parses a TCGdex set answer into its card list.
pub fn parse_set_cards(json: &str) -> Result<Vec<CardBrief>, FetchError> {
    let v: Value = serde_json::from_str(json).map_err(|e| bad(&format!("set JSON: {e}")))?;
    let cards = v
        .get("cards")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("set without a card list"))?;
    cards
        .iter()
        .map(|c| {
            Ok(CardBrief {
                id: c
                    .get("id")
                    .and_then(str_of)
                    .ok_or_else(|| bad("set card without an id"))?,
                local_id: c.get("localId").and_then(str_of).unwrap_or_default(),
                name: c.get("name").and_then(str_of).unwrap_or_default(),
            })
        })
        .collect()
}

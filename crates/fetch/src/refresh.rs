//! Price refresh: fetch each card whose prices are older than `max_age_hours` and keep the
//! quotes as a new dated row (history is never overwritten).

use crate::polite::{storage, FetchError};
use crate::tcgdex::Tcgdex;
use card_ledger_core::card::Lang;
use card_ledger_core::db::Db;
use card_ledger_core::time;
use std::collections::HashSet;

/// After this many "source unavailable" answers with no network success between them, the run
/// stops (cards skipped as fresh or served from the cache don't break the run).
pub const MAX_UNAVAILABLE_IN_A_ROW: usize = 3;

/// What happened to one card.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Fetched from TCGdex and `quotes` price lines saved.
    Fetched {
        quotes: usize,
    },
    /// The card JSON fetched for identity in the last `max_age` hours had prices not yet saved;
    /// they were saved, dated when that JSON was fetched. No request sent.
    FromCache {
        quotes: usize,
    },
    /// Prices are newer than `max_age`: nothing sent.
    SkippedFresh,
    /// TCGdex has no such card (404, now or in the last 7 days).
    Missing,
    Failed(String),
}

/// Progress for one card, handed to the callback as each card finishes.
#[derive(Debug)]
pub struct Progress<'p> {
    /// Cards finished so far, this one included.
    pub done: usize,
    pub total: usize,
    pub lang: Lang,
    pub card_id: &'p str,
    pub step: &'p Step,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct RefreshReport {
    /// Distinct cards asked about.
    pub total: usize,
    pub fetched: usize,
    pub from_cache: usize,
    pub skipped_fresh: usize,
    pub missing: usize,
    pub failed: usize,
    /// Fetched (or from cache) but with no price at all from TCGdex.
    pub no_price: usize,
    /// One line per failure: "en dc1-6: ...".
    pub failures: Vec<String>,
    /// Why the run stopped before the end (rate limited, bot check, source down, database).
    pub stop: Option<FetchError>,
    /// Requests sent in this run.
    pub requests: u32,
}

fn fresh(at: Option<&str>, now: u64, max_age_s: u64) -> bool {
    at.and_then(time::unix_from_iso)
        .is_some_and(|t| now.saturating_sub(t) < max_age_s)
}

/// Refreshes prices for `cards` (duplicates are fetched once, in first-seen order). A card is
/// skipped while its newest quote is under `max_age_hours` old. Otherwise it is fetched from
/// TCGdex, bypassing the 24 h card cache, and its quotes are saved with `fetched_at` = now.
/// A rate limit, a bot check, a database error or [`MAX_UNAVAILABLE_IN_A_ROW`] unavailable
/// answers stop the run; everything saved before that is kept.
pub fn refresh_prices(
    db: &Db,
    lookup: &mut Tcgdex<'_>,
    cards: &[(Lang, String)],
    max_age_hours: u64,
    progress: &mut dyn FnMut(&Progress<'_>),
) -> RefreshReport {
    let max_age_s = max_age_hours.saturating_mul(3600);
    let mut seen = HashSet::new();
    let todo: Vec<&(Lang, String)> = cards
        .iter()
        .filter(|c| seen.insert((c.0, c.1.as_str())))
        .collect();
    let mut report = RefreshReport {
        total: todo.len(),
        ..Default::default()
    };
    let start_requests = lookup.requests();
    let mut unavailable_run = 0;

    for (i, (lang, id)) in todo.iter().enumerate() {
        let (lang, id) = (*lang, id.as_str());
        let step = match one(db, lookup, lang, id, max_age_s) {
            Ok(s) => s,
            Err(e) => {
                let stop = e.stops_run();
                if matches!(e, FetchError::Unavailable(_)) {
                    unavailable_run += 1;
                }
                report.failures.push(format!("{} {id}: {e}", lang.code()));
                if stop || unavailable_run >= MAX_UNAVAILABLE_IN_A_ROW {
                    report.failed += 1;
                    report.stop = Some(e.clone());
                    progress(&Progress {
                        done: i + 1,
                        total: report.total,
                        lang,
                        card_id: id,
                        step: &Step::Failed(e.to_string()),
                    });
                    break;
                }
                Step::Failed(e.to_string())
            }
        };
        // Only a real 2xx from the network shows the source is back; a skip or a cached answer
        // says nothing about it.
        if matches!(step, Step::Fetched { .. }) {
            unavailable_run = 0;
        }
        match &step {
            Step::Fetched { quotes } => {
                report.fetched += 1;
                report.no_price += usize::from(*quotes == 0);
            }
            Step::FromCache { .. } => report.from_cache += 1,
            Step::SkippedFresh => report.skipped_fresh += 1,
            Step::Missing => report.missing += 1,
            Step::Failed(_) => report.failed += 1,
        }
        progress(&Progress {
            done: i + 1,
            total: report.total,
            lang,
            card_id: id,
            step: &step,
        });
    }
    report.requests = lookup.requests() - start_requests;
    report
}

fn one(
    db: &Db,
    lookup: &mut Tcgdex<'_>,
    lang: Lang,
    id: &str,
    max_age_s: u64,
) -> Result<Step, FetchError> {
    let now = lookup.now_secs();
    let last = db.last_price_fetch(lang, id).map_err(storage)?;
    if fresh(last.as_deref(), now, max_age_s) {
        return Ok(Step::SkippedFresh);
    }
    // A card JSON fetched recently (for identity) whose prices were never saved: save them, dated
    // when it was fetched, instead of asking again. With no prices in it, there is nothing newer
    // to get yet either.
    if let Some((card, at)) = lookup.cached_card(lang, id)? {
        let newer_than_quotes = match (
            time::unix_from_iso(&at),
            last.as_deref().and_then(time::unix_from_iso),
        ) {
            (Some(c), Some(q)) => c > q,
            (Some(_), None) => true,
            _ => false,
        };
        if newer_than_quotes && fresh(Some(&at), now, max_age_s) {
            if card.prices.is_empty() {
                return Ok(Step::SkippedFresh);
            }
            db.save_quotes(lang, id, &card.prices, &at)
                .map_err(storage)?;
            return Ok(Step::FromCache {
                quotes: card.prices.len(),
            });
        }
    }
    match lookup.card_fresh(lang, id)? {
        None => Ok(Step::Missing),
        Some(card) => {
            db.save_quotes(lang, id, &card.prices, &lookup.now_iso())
                .map_err(storage)?;
            Ok(Step::Fetched {
                quotes: card.prices.len(),
            })
        }
    }
}

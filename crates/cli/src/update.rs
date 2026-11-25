//! "Update": match any new cards to the catalogue, then refresh prices that are over a day old,
//! then (only with a JustTCG key) per-condition prices. Shared by `card-ledger prices` and the
//! window's Refresh button.

use card_ledger_core::db::Db;
use card_ledger_core::ledger;
use card_ledger_fetch::justtcg::keystore;
use card_ledger_fetch::{refresh_prices, Http, JustTcg, SystemClock, Tcgdex};
use std::path::Path;

/// `prices: false` stops after matching (`card-ledger match`): nothing is priced and no price
/// is saved, though the card answers TCGdex sends are cached as usual.
pub fn run(
    data_dir: &Path,
    max_age_hours: u64,
    resolve_all: bool,
    echo: bool,
    prices: bool,
) -> Result<String, String> {
    let path = data_dir.join("ledger.db");
    let db = Db::open(&path).map_err(|e| format!("can't open {}: {e}", path.display()))?;
    let http = Http::new();
    let clock = SystemClock;
    let mut lines = Vec::new();

    let mut tcgdex = Tcgdex::new(&db, &http, &clock);
    let rr = ledger::resolve_all(&db, &mut tcgdex, !resolve_all, &mut |i, n, item| {
        if echo && (i % 25 == 0 || i == n) {
            eprintln!("  matching {i}/{n}: {}", item.product_name);
        }
    })
    .map_err(|e| format!("matching stopped: {e}"))?;
    lines.push(format!(
        "Matched {} card lines, {} unresolved, {} with flags.",
        rr.resolved, rr.unresolved, rr.flagged
    ));
    if rr.skipped > 0 {
        lines.push(format!(
            "{} line(s) skipped while TCGdex was briefly unavailable; the next run matches them.",
            rr.skipped
        ));
    }
    if let Some(e) = &rr.stopped {
        lines.push(format!(
            "Matching stopped early: {e}. Run it again later; nothing was lost."
        ));
        return Ok(lines.join("\n"));
    }
    if !prices {
        return Ok(lines.join("\n"));
    }

    let targets = ledger::price_targets(&db).map_err(|e| e.to_string())?;
    let rep = refresh_prices(&db, &mut tcgdex, &targets, max_age_hours, &mut |p| {
        if echo && (p.done % 25 == 0 || p.done == p.total) {
            eprintln!("  prices {}/{}", p.done, p.total);
        }
    });
    lines.push(format!(
        "Prices: {} fetched, {} from today's cache, {} already fresh, {} without a price, {} missing, {} failed ({} requests to TCGdex).",
        rep.fetched, rep.from_cache, rep.skipped_fresh, rep.no_price, rep.missing, rep.failed, rep.requests
    ));
    for f in rep.failures.iter().take(5) {
        lines.push(format!("  {f}"));
    }
    if let Some(stop) = &rep.stop {
        lines.push(format!(
            "Price refresh stopped early: {stop}. What was fetched is saved."
        ));
    }

    let store = keystore::default_store(data_dir);
    match JustTcg::from_config(store.as_ref(), &http, &clock) {
        Err(e) => lines.push(format!("Condition prices: off ({e}).")),
        Ok(None) => lines.push(
            "Condition prices: off (no JustTCG key set; see `card-ledger justtcg-key set`).".into(),
        ),
        Ok(Some(mut j)) => {
            let cr = j.refresh_conditions(&db, &targets, max_age_hours, &mut |b| {
                if echo {
                    eprintln!("  condition prices batch {}/{}", b.batch, b.batches);
                }
            });
            lines.push(format!(
                "Condition prices: {} cards priced, {} rows saved, {} already fresh, {} not found, {} requests.",
                cr.cards_priced, cr.saved, cr.skipped_fresh, cr.not_found, cr.requests
            ));
            if let Some(stop) = &cr.stop {
                lines.push(format!("Condition prices stopped: {stop}"));
            }
        }
    }
    Ok(lines.join("\n"))
}

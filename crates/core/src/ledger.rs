//! The ledger view: everything the window and the CLI show, one row per physical card.
//! The window renders `LedgerView` as JSON.

use crate::card::{CardLookup, Flag, FlagCode, Lang, LookupError, Resolution};
use crate::db::{Db, Item};
use crate::identity::{self, SetMap};
use crate::model::Condition;
use crate::piles::{self, PileInput};
use crate::pricing::{self, ConditionConfig, NmSource, Research};
use serde::Serialize;
use std::collections::HashMap;

/// One physical card, fully worked out.
#[derive(Debug, Clone, Serialize)]
pub struct CardView {
    pub copy_id: i64,
    pub item_id: i64,
    pub copy_no: u32,
    /// How many copies of this item the ledger holds.
    pub copies_of_item: u32,
    pub set_name: String,
    pub product_name: String,
    pub card_number: String,
    pub rarity: String,
    /// Collectr's variant label ("Reverse Holofoil").
    pub variant: String,
    /// "dc1-6/holo", or None when unresolved.
    pub canonical: Option<String>,
    /// Catalogue name when resolved.
    pub catalog_name: Option<String>,
    pub dex_id: Option<u32>,
    /// Catalogue image base URL (TCGdex: append "/low.webp" or "/high.webp").
    pub image: Option<String>,
    /// "unchecked", "NM", "LP", "MP", "HP", "DMG".
    pub condition: String,
    pub pulled: bool,
    pub note: String,
    /// Value used for this card now (condition-adjusted; NM for unchecked, see README "Piles").
    pub value: Option<f64>,
    /// Range for the current condition (research ranges), or NM..HP for unchecked.
    pub value_low: Option<f64>,
    pub value_high: Option<f64>,
    /// Plain words: where `value` came from, e.g. "Research LP range (2026-11-24)" or
    /// "TCGplayer NM market x 0.54 (LP estimate)".
    pub value_basis: String,
    /// The NM price and its label with source and date, e.g. "TCGdex · TCGplayer NM market · 2026-11-24".
    pub nm_price: Option<f64>,
    pub nm_label: Option<String>,
    /// Collectr's own figure, always labelled "Collectr (NM)" in the window.
    pub collectr_price: Option<f64>,
    pub collectr_price_date: Option<String>,
    /// Values for every condition, for the picker: [NM, LP, MP, HP, DMG].
    pub by_condition: [Option<f64>; 5],
    /// What this card adds to the totals: its value, or its LP value while unchecked.
    pub value_counted: Option<f64>,
    /// False when the latest Collectr export no longer lists this line.
    pub in_last_export: bool,
    /// "toploader", "small-binder", "big-binder", "box", "set-aside".
    pub pile: String,
    pub pile_reason: String,
    /// Unchecked and a worse condition would move it to a cheaper pile.
    pub check_condition: bool,
    pub flags: Vec<Flag>,
    pub unresolved_reason: Option<String>,
    /// Research notes, for cards with a research range.
    pub research: Option<ResearchView>,
    /// Sort key inside its pile (Pokédex order for the big binder).
    pub order: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResearchView {
    pub confidence: String,
    pub check: String,
    pub grade: String,
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PileTotal {
    pub pile: String,
    pub cards: u32,
    pub pulled: u32,
    /// Unchecked cards at LP (as the headline).
    pub value: f64,
    /// Unchecked cards at NM.
    pub value_if_nm: f64,
}

/// The group for copies of lines the latest export no longer lists.
pub const NOT_IN_EXPORT: &str = "not-in-export";

#[derive(Debug, Clone, Default, Serialize)]
pub struct LedgerView {
    pub cards: Vec<CardView>,
    pub piles: Vec<PileTotal>,
    pub total_cards: u32,
    pub unchecked: u32,
    pub unresolved: u32,
    pub flagged: u32,
    /// Copies of lines the latest Collectr export no longer lists (not in piles or totals).
    pub not_in_export: u32,
    pub total_value: f64,
    /// The same total with every unchecked card at its LP value (binder wear assumed), so the
    /// headline isn't an all-Near-Mint figure while cards are still unchecked.
    pub total_value_unchecked_lp: f64,
    /// Sum of Collectr's NM figures, for comparison.
    pub collectr_total: f64,
    /// Newest price fetch time, if any.
    pub prices_fetched_at: Option<String>,
    pub collectr_price_date: Option<String>,
}

/// Matches every item to a catalogue card and saves the result. Stops at the first lookup
/// error (the source is down or asked us to wait); what was saved so far stays saved.
pub struct ResolveReport {
    pub resolved: u32,
    pub unresolved: u32,
    pub flagged: u32,
    /// Lines skipped because the catalogue was briefly unavailable (matched on the next run).
    pub skipped: u32,
    pub stopped: Option<LookupError>,
}

/// Unavailable answers (each already retried once by the fetcher) that stop a matching run.
pub const MAX_DOWN_PER_RUN: u32 = 3;

/// `only_missing`: skip items that already have a saved resolution.
pub fn resolve_all(
    db: &Db,
    lookup: &mut dyn CardLookup,
    only_missing: bool,
    progress: &mut dyn FnMut(usize, usize, &Item),
) -> rusqlite::Result<ResolveReport> {
    let sets = SetMap::embedded();
    let items = db.items()?;
    let done: HashMap<i64, Resolution> = db
        .resolutions()?
        .into_iter()
        .map(|r| (r.item_id, r))
        .collect();
    let mut rep = ResolveReport {
        resolved: 0,
        unresolved: 0,
        flagged: 0,
        skipped: 0,
        stopped: None,
    };
    // Counted over the whole run, not "in a row": answers served from the cache say nothing
    // about whether the source is up, so they must not reset the count.
    let mut down = 0;
    for (i, item) in items.iter().enumerate() {
        progress(i + 1, items.len(), item);
        let r = match done.get(&item.id) {
            Some(r) if only_missing => r.clone(),
            _ => match identity::resolve(item, &sets, lookup) {
                Ok(r) => {
                    db.save_resolution(&r)?;
                    r
                }
                // A passing outage skips this line (left for the next run, never marked
                // unresolved); the third in one run, or being told to slow down, stops it.
                Err(LookupError::Unavailable(_)) if down + 1 < MAX_DOWN_PER_RUN => {
                    down += 1;
                    rep.skipped += 1;
                    continue;
                }
                Err(e) => {
                    rep.stopped = Some(e);
                    break;
                }
            },
        };
        if r.card.is_some() {
            rep.resolved += 1;
        } else {
            rep.unresolved += 1;
        }
        if !r.flags.is_empty() {
            rep.flagged += 1;
        }
    }
    Ok(rep)
}

/// The distinct catalogue cards the ledger holds, for a price refresh.
pub fn price_targets(db: &Db) -> rusqlite::Result<Vec<(Lang, String)>> {
    let mut out: Vec<(Lang, String)> = db
        .resolutions()?
        .into_iter()
        .filter_map(|r| r.card.map(|c| (c.lang, c.card_id)))
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// A promo: Collectr says so (rarity or set name), or the catalogue set is a promo set
/// (English "xyp", "bwp", "mcd14"; Japanese "SM-P", "S-P", "SV-P").
pub fn is_promo(item: &Item, res: Option<&Resolution>) -> bool {
    let r = item.rarity.to_ascii_lowercase();
    let set = item.set_name.to_ascii_lowercase();
    let set_id_promo = res.and_then(|r| r.card.as_ref()).is_some_and(|c| {
        let id = c.card_id.to_ascii_lowercase();
        let first = id.split('-').next().unwrap_or("");
        first.ends_with('p')
            || first.starts_with("mcd")
            || first.contains("promo")
            || id.contains("-p-")
    });
    r.contains("promo") || set.contains("promo") || set_id_promo
}

/// Collectr's figure can stand in for an NM price only when Collectr itself says the line is
/// an ungraded Near Mint copy; otherwise it is an LP price, or a graded slab's price.
fn collectr_is_raw_nm(item: &Item) -> bool {
    let cond = item.collectr_condition.trim();
    let grade = item.grade.trim();
    (cond.is_empty() || cond.eq_ignore_ascii_case("near mint") || cond.eq_ignore_ascii_case("nm"))
        && (grade.is_empty() || grade.eq_ignore_ascii_case("ungraded"))
}

fn flag(code: FlagCode, message: String) -> Flag {
    Flag { code, message }
}

fn image_for(db: &Db, lang: Lang, card_id: &str) -> Option<String> {
    let (json, _) = db.cache_get("card", lang, card_id).ok()??;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    let img = v.get("image")?.as_str()?;
    img.starts_with("https://assets.tcgdex.net/")
        .then(|| img.to_string())
}

/// Builds the full view from the store: identity, prices, condition values, piles and flags.
pub fn build_view(db: &Db) -> rusqlite::Result<LedgerView> {
    build_view_with(db, Research::embedded())
}

/// `build_view` with a given set of research ranges (tests pass a made-up fixture; the build
/// embeds `data/research-values.json`, which may be empty).
pub fn build_view_with(db: &Db, research: &Research) -> rusqlite::Result<LedgerView> {
    let cfg = ConditionConfig::embedded();
    let items = db.items()?;
    let copies = db.copies()?;
    let resolutions: HashMap<i64, Resolution> = db
        .resolutions()?
        .into_iter()
        .map(|r| (r.item_id, r))
        .collect();

    // Per item: everything that doesn't depend on the copy's condition.
    struct ItemCtx {
        by: pricing::ByCondition,
        nm: Option<pricing::NmQuote>,
        flags: Vec<Flag>,
        research: Option<ResearchView>,
        image: Option<String>,
        newest_fetch: Option<String>,
    }
    let mut ctx: HashMap<i64, ItemCtx> = HashMap::new();
    for it in &items {
        let res = resolutions.get(&it.id);
        let card = res.and_then(|r| r.card.as_ref());
        let (quotes, cond_prices, image) = match card {
            Some(c) => (
                db.latest_quotes(c.lang, &c.card_id)?,
                db.latest_condition_prices(c.lang, &c.card_id, c.variant)?,
                image_for(db, c.lang, &c.card_id),
            ),
            None => (vec![], None, None),
        };
        let mut flags = res.map(|r| r.flags.clone()).unwrap_or_default();
        let mut extra: Vec<Flag> = Vec::new();
        let raw_nm = collectr_is_raw_nm(it);
        if !raw_nm && it.collectr_price.is_some() {
            extra.push(flag(
                FlagCode::CollectrNotNm,
                format!(
                    "Collectr lists this line as {} / {}, so its ${:.2} isn't used as a Near Mint price.",
                    it.collectr_condition.trim(),
                    it.grade.trim(),
                    it.collectr_price.unwrap_or(0.0)
                ),
            ));
        }
        let collectr = if raw_nm {
            it.collectr_price
                .map(|p| (p, it.collectr_price_date.clone()))
        } else {
            None
        };
        // A Japanese card matched in an English set must not take the English card's price.
        let jp_on_en = card.is_some_and(|k| k.lang == Lang::En) && identity::looks_japanese(it);
        let usable_quotes: &[crate::db::StoredQuote] = if jp_on_en { &[] } else { &quotes };
        let nm = pricing::pick_nm_quote(it.variant, usable_quotes, collectr);
        let found = research.find(&it.product_name, &it.card_number, &it.set_name, it.variant);
        let by = pricing::valuate_all(nm.as_ref(), found, cond_prices.as_ref(), cfg);
        let nm_usd = nm
            .as_ref()
            .filter(|q| q.source == NmSource::Tcgplayer)
            .map(|q| q.price);
        if res.is_some() {
            extra.extend(identity::price_flags(it, card, nm_usd));
        }
        // Collectr's figure far outside the researched NM range is usually a wrong printing.
        if let (Some(e), Some(p)) = (found, it.collectr_price.filter(|p| *p > 0.0)) {
            if p > e.nm[1] * 3.0 || p * 3.0 < e.nm[0] {
                extra.push(flag(
                    FlagCode::ResearchDisagrees,
                    format!(
                        "Collectr's ${p:.2} is far from the researched Near Mint range ${:.2}-${:.2}; check the printing and number on the card.",
                        e.nm[0], e.nm[1]
                    ),
                ));
            }
        }
        for f in extra {
            if !flags.iter().any(|g| g.code == f.code) {
                flags.push(f);
            }
        }
        let newest_fetch = quotes.iter().map(|q| q.fetched_at.clone()).max();
        ctx.insert(
            it.id,
            ItemCtx {
                by,
                nm,
                flags,
                research: found.map(|e| ResearchView {
                    confidence: e.conf.clone(),
                    check: e.check.clone(),
                    grade: e.grade.clone(),
                    note: e.note.clone(),
                }),
                image,
                newest_fetch,
            },
        );
    }

    let item_by_id: HashMap<i64, &Item> = items.iter().map(|i| (i.id, i)).collect();
    let mut held: HashMap<i64, u32> = HashMap::new();
    for c in &copies {
        *held.entry(c.item_id).or_default() += 1;
    }

    let mut inputs = Vec::new();
    // Copies of lines the latest Collectr export no longer lists (e.g. corrected in Collectr):
    // kept, shown in their own group, left out of the piles and totals.
    let mut gone: Vec<&crate::db::Copy> = Vec::new();
    for c in &copies {
        let Some(it) = item_by_id.get(&c.item_id) else {
            continue;
        };
        if !it.in_last_export {
            gone.push(c);
            continue;
        }
        let x = &ctx[&it.id];
        let res = resolutions.get(&it.id);
        let card = res.and_then(|r| r.card.as_ref());
        let val = pricing::value_for(&x.by, c.condition);
        let not_pokemon = x.flags.iter().any(|f| f.code == FlagCode::NotPokemon)
            || !identity::is_pokemon_category(&it.category)
            || !identity::is_pokemon_catalog_category(card.and_then(|k| k.category.as_deref()));
        inputs.push(PileInput {
            copy_id: c.id,
            // Copies of the same card share a group (one keeper, the rest are extras). A graded
            // slab is never an extra copy of the raw card, so graded lines get their own group.
            group: {
                let base = card.map_or_else(|| format!("item:{}", it.id), |k| k.canonical.clone());
                let grade = it.grade.trim();
                if grade.is_empty() || grade.eq_ignore_ascii_case("ungraded") {
                    base
                } else {
                    format!("{base}|{grade}")
                }
            },
            copy_no: c.copy_no,
            condition: c.condition,
            value: val.value,
            worst_value: x.by.hp.value,
            variant: it.variant,
            rarity: it.rarity.clone(),
            is_promo: is_promo(it, res),
            is_pokemon: !not_pokemon,
            dex_id: card.and_then(|k| k.dex_id),
            name: card.map_or_else(|| it.product_name.clone(), |k| k.name.clone()),
            set_name: it.set_name.clone(),
            number: it.card_number.clone(),
        });
    }
    let outcomes = piles::assign(&inputs, cfg);

    let mut v = LedgerView {
        collectr_price_date: items.iter().find_map(|i| i.collectr_price_date.clone()),
        ..Default::default()
    };
    let mut totals: Vec<PileTotal> = piles::Pile::ALL
        .iter()
        .map(|p| PileTotal {
            pile: p.code().to_string(),
            ..Default::default()
        })
        .collect();
    totals.push(PileTotal {
        pile: NOT_IN_EXPORT.into(),
        ..Default::default()
    });
    // Piled copies first (with their outcomes), then the not-in-export ones.
    let rows: Vec<(&crate::db::Copy, Option<&piles::PileOutcome>)> = inputs
        .iter()
        .zip(outcomes.iter())
        .map(|(input, out)| {
            (
                copies
                    .iter()
                    .find(|c| c.id == input.copy_id)
                    .expect("copy from input"),
                Some(out),
            )
        })
        .chain(gone.iter().map(|c| (*c, None)))
        .collect();
    for (c, out) in rows {
        let it = item_by_id[&c.item_id];
        let x = &ctx[&it.id];
        let res = resolutions.get(&it.id);
        let card = res.and_then(|r| r.card.as_ref());
        let val = pricing::value_for(&x.by, c.condition);
        // `value_for` already words the unchecked case ("unchecked, priced as NM (...)").
        let value_basis = if c.condition == Condition::Unchecked {
            format!("{}; range runs from HP to NM", val.basis)
        } else {
            val.basis.clone()
        };
        // Totals count unchecked cards at their LP value (binder wear assumed), the same way
        // as the headline; piles themselves still use the NM value (README "Piles").
        let counted = if c.condition == Condition::Unchecked {
            x.by.lp.value
        } else {
            val.value
        };
        let (pile, reason, check, order) = match out {
            Some(o) => (
                o.pile.code().to_string(),
                o.reason.clone(),
                o.check_condition,
                o.order.clone(),
            ),
            None => (
                NOT_IN_EXPORT.to_string(),
                "not in the latest Collectr export (kept; see `card-ledger carry-over`)"
                    .to_string(),
                false,
                format!("{} {}", it.set_name, it.card_number),
            ),
        };
        if let Some(t) = totals.iter_mut().find(|t| t.pile == pile) {
            t.cards += 1;
            t.pulled += c.pulled as u32;
            t.value += counted.unwrap_or(0.0);
            t.value_if_nm += val.value.unwrap_or(0.0);
        }
        let in_totals = out.is_some_and(|o| o.pile != piles::Pile::SetAside);
        if in_totals {
            v.total_value += val.value.unwrap_or(0.0);
            v.total_value_unchecked_lp += counted.unwrap_or(0.0);
            v.collectr_total += it.collectr_price.unwrap_or(0.0);
        }
        v.cards.push(CardView {
            copy_id: c.id,
            item_id: it.id,
            copy_no: c.copy_no,
            copies_of_item: held[&it.id],
            set_name: it.set_name.clone(),
            product_name: it.product_name.clone(),
            card_number: it.card_number.clone(),
            rarity: it.rarity.clone(),
            variant: it.variant.label().to_string(),
            canonical: card.map(|k| k.canonical.clone()),
            catalog_name: card.map(|k| k.name.clone()),
            dex_id: card.and_then(|k| k.dex_id),
            image: x.image.clone(),
            condition: c.condition.code().to_string(),
            pulled: c.pulled,
            note: c.note.clone(),
            value: val.value,
            value_low: val.low,
            value_high: val.high,
            value_basis,
            nm_price: x.nm.as_ref().map(|q| q.price),
            nm_label: x.nm.as_ref().map(|q| q.label.clone()),
            collectr_price: it.collectr_price,
            collectr_price_date: it.collectr_price_date.clone(),
            by_condition: x.by.values(),
            value_counted: counted,
            in_last_export: it.in_last_export,
            pile,
            pile_reason: reason,
            check_condition: check,
            flags: x.flags.clone(),
            unresolved_reason: match res {
                None => Some("not matched yet (run `card-ledger prices`)".into()),
                Some(r) if r.card.is_none() => {
                    Some(r.reason.clone().unwrap_or_else(|| "no match".into()))
                }
                Some(_) => None,
            },
            research: x.research.clone(),
            order,
        });
        if let Some(f) = &x.newest_fetch {
            if v.prices_fetched_at.as_ref().is_none_or(|p| f > p) {
                v.prices_fetched_at = Some(f.clone());
            }
        }
    }
    for t in &mut totals {
        t.value = (t.value * 100.0).round() / 100.0;
        t.value_if_nm = (t.value_if_nm * 100.0).round() / 100.0;
    }
    v.not_in_export = totals
        .iter()
        .find(|t| t.pile == NOT_IN_EXPORT)
        .map_or(0, |t| t.cards);
    v.total_value = (v.total_value * 100.0).round() / 100.0;
    v.total_value_unchecked_lp = (v.total_value_unchecked_lp * 100.0).round() / 100.0;
    v.collectr_total = (v.collectr_total * 100.0).round() / 100.0;
    v.piles = totals;
    // Counts are of the cards in the latest export; not-in-export copies have their own count.
    let count = |f: &dyn Fn(&CardView) -> bool| {
        v.cards.iter().filter(|c| c.in_last_export && f(c)).count() as u32
    };
    let (total, unchecked, unresolved, flagged) = (
        count(&|_| true),
        count(&|c| c.condition == "unchecked"),
        count(&|c| c.unresolved_reason.is_some()),
        count(&|c| !c.flags.is_empty()),
    );
    (v.total_cards, v.unchecked, v.unresolved, v.flagged) = (total, unchecked, unresolved, flagged);
    v.cards
        .sort_by(|a, b| a.pile.cmp(&b.pile).then(a.order.cmp(&b.order)));
    Ok(v)
}

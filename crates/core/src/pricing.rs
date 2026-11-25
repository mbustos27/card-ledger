//! Condition pricing.
//!
//! Three things live here:
//! - [`ConditionConfig`]: the multipliers and pile thresholds from `data/conditions.toml`.
//! - [`Research`]: optional per-card NM/LP/MP ranges (`data/research-values.json`).
//! - [`pick_nm_quote`] and [`valuate_all`]: the NM price for one printing, then a value for every
//!   condition, each with a plain-words basis saying where it came from and when.

use crate::db::{StoredConditionPrices, StoredQuote};
use crate::model::{Condition, Variant};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------------------------

const CONDITIONS_TOML: &str = include_str!("../../../data/conditions.toml");
const RESEARCH_JSON: &str = include_str!("../../../data/research-values.json");

/// The day the research ranges were read.
pub const RESEARCH_DATE: &str = "2026-11-24";

/// A multiplier of the NM price, with the fair band around it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Multiplier {
    pub value: f64,
    pub low: f64,
    pub high: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Multipliers {
    pub lp: Multiplier,
    pub mp: Multiplier,
    pub hp: Multiplier,
    pub dmg: Multiplier,
}

/// Factors that turn a researched card's MP range into HP and Damaged ranges.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchDerived {
    pub hp_from_mp: f64,
    pub dmg_from_mp: f64,
}

/// Pile thresholds in USD.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PileThresholds {
    /// value >= this goes in a toploader.
    pub toploader: f64,
    /// value >= this (and below `toploader`) goes in the small binder.
    pub small_binder: f64,
}

/// Everything `data/conditions.toml` holds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionConfig {
    pub multipliers: Multipliers,
    pub research_derived: ResearchDerived,
    pub piles: PileThresholds,
}

impl ConditionConfig {
    /// The config built into the program from `data/conditions.toml`.
    pub fn embedded() -> &'static ConditionConfig {
        static CFG: OnceLock<ConditionConfig> = OnceLock::new();
        CFG.get_or_init(|| {
            ConditionConfig::parse(CONDITIONS_TOML).expect("data/conditions.toml is valid")
        })
    }

    /// Parses and checks a conditions file: every band holds its value, every factor is in
    /// (0, 1], worse conditions never price above better ones, and the binder line sits below
    /// the toploader line.
    pub fn parse(text: &str) -> Result<ConditionConfig, String> {
        let cfg: ConditionConfig =
            toml::from_str(text).map_err(|e| format!("conditions file: {e}"))?;
        let m = &cfg.multipliers;
        for (name, x) in [("lp", m.lp), ("mp", m.mp), ("hp", m.hp), ("dmg", m.dmg)] {
            let ok = [x.value, x.low, x.high]
                .iter()
                .all(|v| v.is_finite() && *v > 0.0 && *v <= 1.0)
                && x.low <= x.value
                && x.value <= x.high;
            if !ok {
                return Err(format!(
                    "conditions file: multipliers.{name} needs 0 < low <= value <= high <= 1"
                ));
            }
        }
        if !(m.lp.value >= m.mp.value && m.mp.value >= m.hp.value && m.hp.value >= m.dmg.value) {
            return Err("conditions file: multipliers must fall from LP to DMG".into());
        }
        let d = &cfg.research_derived;
        for (name, v) in [("hp_from_mp", d.hp_from_mp), ("dmg_from_mp", d.dmg_from_mp)] {
            if !(v.is_finite() && v > 0.0 && v <= 1.0) {
                return Err(format!(
                    "conditions file: research_derived.{name} must be in (0, 1]"
                ));
            }
        }
        if d.dmg_from_mp > d.hp_from_mp {
            return Err("conditions file: dmg_from_mp must not exceed hp_from_mp".into());
        }
        let p = &cfg.piles;
        if !(p.small_binder.is_finite()
            && p.toploader.is_finite()
            && 0.0 < p.small_binder
            && p.small_binder < p.toploader)
        {
            return Err("conditions file: need 0 < piles.small_binder < piles.toploader".into());
        }
        Ok(cfg)
    }

    fn multiplier(&self, c: Condition) -> Option<Multiplier> {
        let m = &self.multipliers;
        match c {
            Condition::LightlyPlayed => Some(m.lp),
            Condition::ModeratelyPlayed => Some(m.mp),
            Condition::HeavilyPlayed => Some(m.hp),
            Condition::Damaged => Some(m.dmg),
            Condition::NearMint | Condition::Unchecked => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Research
// ---------------------------------------------------------------------------------------------

/// One researched card: realistic buyer-price ranges in USD for NM, LP and MP.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchEntry {
    /// Product name as Collectr writes it ("Bidoof (71)").
    #[serde(rename = "n")]
    pub name: String,
    /// Card number as Collectr writes it ("71/99", "XY150").
    #[serde(rename = "no")]
    pub number: String,
    /// Collectr's set name for the researched line ("Double Crisis"). When present, a lookup
    /// must come from the same set.
    #[serde(default)]
    pub set: Option<String>,
    /// Collectr's variant label for the researched line ("Holofoil"). When present, a lookup
    /// must be the same printing (a 1st Edition range never prices an Unlimited copy).
    #[serde(default)]
    pub variant: Option<String>,
    pub nm: [f64; 2],
    pub lp: [f64; 2],
    pub mp: [f64; 2],
    /// "high", "medium" or "low".
    pub conf: String,
    /// What to look at on the physical card before trusting the price.
    pub check: String,
    /// Worth grading: "yes", "maybe" or "no".
    pub grade: String,
    pub note: String,
    /// The day the ranges were read (not in the JSON; filled from the file's date).
    #[serde(default)]
    pub date: String,
}

/// The research ranges, looked up by Collectr name and number.
#[derive(Debug, Clone, PartialEq)]
pub struct Research {
    pub entries: Vec<ResearchEntry>,
}

impl Research {
    /// The research built into the program (`data/research-values.json`; edit it and rebuild).
    pub fn embedded() -> &'static Research {
        static R: OnceLock<Research> = OnceLock::new();
        R.get_or_init(|| Research::parse(RESEARCH_JSON).expect("research JSON is valid"))
    }

    /// Parses a research JSON array. Every range must be finite, non-negative and low <= high.
    /// Entries are dated [`RESEARCH_DATE`].
    pub fn parse(json: &str) -> Result<Research, String> {
        let mut entries: Vec<ResearchEntry> =
            serde_json::from_str(json).map_err(|e| format!("research file: {e}"))?;
        for e in &mut entries {
            for (what, r) in [("nm", e.nm), ("lp", e.lp), ("mp", e.mp)] {
                if !(r[0].is_finite() && r[1].is_finite() && 0.0 <= r[0] && r[0] <= r[1]) {
                    return Err(format!(
                        "research file: {} #{}: bad {what} range",
                        e.name, e.number
                    ));
                }
            }
            if e.date.is_empty() {
                e.date = RESEARCH_DATE.to_string();
            }
        }
        Ok(Research { entries })
    }

    /// Finds the entry for a card, matched on normalised number plus name. Numbers lose any
    /// "/total" and leading zeros ("040/087" = "40"). Names ignore case, punctuation and a
    /// trailing "(…)" such as "(71)" or "(142 Full Art)", but a "(JP)" card only matches a
    /// "(JP)" entry. An exact name match wins; a match only after dropping the brackets must be
    /// the only one.
    ///
    /// When an entry records its set and printing, both must match too, so a range researched
    /// for one printing or set never prices another card with the same name and number.
    pub fn find(
        &self,
        product_name: &str,
        card_number: &str,
        set_name: &str,
        variant: Variant,
    ) -> Option<&ResearchEntry> {
        let no = norm_number(card_number);
        if no.is_empty() {
            return None;
        }
        let full = norm_name(product_name);
        let base = norm_name(&strip_brackets(product_name));
        let jp = is_jp(product_name);
        let same_no: Vec<&ResearchEntry> = self
            .entries
            .iter()
            .filter(|e| norm_number(&e.number) == no && is_jp(&e.name) == jp)
            .filter(|e| {
                e.set
                    .as_deref()
                    .is_none_or(|s| s.trim().eq_ignore_ascii_case(set_name.trim()))
            })
            .filter(|e| {
                e.variant
                    .as_deref()
                    .is_none_or(|v| Variant::parse(v) == Some(variant))
            })
            .collect();
        if let Some(e) = same_no.iter().find(|e| norm_name(&e.name) == full) {
            return Some(e);
        }
        let mut base_hits = same_no
            .iter()
            .filter(|e| norm_name(&strip_brackets(&e.name)) == base);
        match (base_hits.next(), base_hits.next()) {
            (Some(e), None) => Some(e),
            _ => None,
        }
    }
}

/// "040/087" -> "40", "rc09" -> "RC9", "XY150" stays, "LOB-062" stays.
pub fn norm_number(s: &str) -> String {
    let head = s
        .trim()
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    let split = head
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(head.len());
    let (prefix, digits) = head.split_at(split);
    if !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && prefix.chars().all(|c| c.is_ascii_alphabetic())
    {
        let trimmed = digits.trim_start_matches('0');
        format!("{prefix}{}", if trimmed.is_empty() { "0" } else { trimmed })
    } else {
        head
    }
}

/// Lowercase, accents folded, apostrophes dropped, everything else non-alphanumeric as a space.
fn norm_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = match c {
            'é' | 'É' | 'è' | 'ê' => 'e',
            '\'' | '’' | '‘' => continue,
            c => c,
        };
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drops every "(…)" group, nested brackets included.
fn strip_brackets(s: &str) -> String {
    let mut depth = 0u32;
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn is_jp(name: &str) -> bool {
    name.to_ascii_uppercase().contains("(JP)")
}

// ---------------------------------------------------------------------------------------------
// NM quote
// ---------------------------------------------------------------------------------------------

/// Where an NM price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum NmSource {
    /// TCGplayer's NM market price, as TCGdex carries it.
    Tcgplayer,
    /// Collectr's own figure from the export.
    Collectr,
}

impl NmSource {
    /// Short words for a basis phrase ("TCGplayer NM").
    pub fn short(self) -> &'static str {
        match self {
            NmSource::Tcgplayer => "TCGplayer NM",
            NmSource::Collectr => "Collectr NM",
        }
    }
}

/// The NM price used for one printing, with its label ("TCGdex · TCGplayer NM market · 2026-11-24").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NmQuote {
    pub price: f64,
    pub label: String,
    /// YYYY-MM-DD, when known.
    pub date: Option<String>,
    pub source: NmSource,
}

/// Picks the NM price for `variant`: the TCGplayer USD bucket whose printing is exactly this
/// variant (newest if several), else Collectr's figure. A bucket for another printing, a bucket
/// whose printing isn't known, Cardmarket and any non-USD figure are never used.
pub fn pick_nm_quote(
    variant: Variant,
    quotes: &[StoredQuote],
    collectr: Option<(f64, Option<String>)>,
) -> Option<NmQuote> {
    let best = quotes
        .iter()
        .filter(|q| {
            q.quote.source.eq_ignore_ascii_case("tcgplayer")
                && q.quote.currency.eq_ignore_ascii_case("USD")
                && q.quote.variant == Some(variant)
                && q.quote.market.is_some_and(|m| m.is_finite() && m > 0.0)
        })
        .max_by_key(|q| date_key(q));
    if let Some(q) = best {
        let date = q
            .quote
            .updated
            .as_deref()
            .and_then(day_of)
            .or_else(|| day_of(&q.fetched_at));
        let label = match &date {
            Some(d) => format!("TCGdex · TCGplayer NM market · {d}"),
            None => "TCGdex · TCGplayer NM market".to_string(),
        };
        return Some(NmQuote {
            price: round2(q.quote.market.unwrap_or_default()),
            label,
            date,
            source: NmSource::Tcgplayer,
        });
    }
    let (price, date) = collectr?;
    if !(price.is_finite() && price > 0.0) {
        return None;
    }
    let date = date.as_deref().and_then(day_of);
    let label = match &date {
        Some(d) => format!("Collectr (NM) · {d}"),
        None => "Collectr (NM)".to_string(),
    };
    Some(NmQuote {
        price: round2(price),
        label,
        date,
        source: NmSource::Collectr,
    })
}

/// Newest-first key for a stored quote: the source's own update day, then our fetch time.
fn date_key(q: &StoredQuote) -> String {
    let up = q
        .quote
        .updated
        .as_deref()
        .and_then(day_of)
        .unwrap_or_default();
    format!("{up}|{}", q.fetched_at)
}

/// The YYYY-MM-DD at the start of a timestamp, if it has one.
fn day_of(s: &str) -> Option<String> {
    let t = s.trim();
    let d = t.get(..10)?;
    let b = d.as_bytes();
    let ok = b.iter().enumerate().all(|(i, c)| match i {
        4 | 7 => *c == b'-',
        _ => c.is_ascii_digit(),
    });
    ok.then(|| d.to_string())
}

// ---------------------------------------------------------------------------------------------
// Valuation
// ---------------------------------------------------------------------------------------------

/// A value for one condition. `value` is `None` when nothing prices the card ("no price yet").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Valuation {
    pub value: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    /// One plain phrase: "Research LP range, 2026-11-24", "TCGplayer NM × 0.54 (estimate)".
    pub basis: String,
}

impl Valuation {
    fn none() -> Valuation {
        Valuation {
            value: None,
            low: None,
            high: None,
            basis: "no price yet".into(),
        }
    }
    fn of(value: f64, low: f64, high: f64, basis: String) -> Valuation {
        Valuation {
            value: Some(round2(value)),
            low: Some(round2(low)),
            high: Some(round2(high)),
            basis,
        }
    }
}

/// Values for every judged condition.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ByCondition {
    pub nm: Valuation,
    pub lp: Valuation,
    pub mp: Valuation,
    pub hp: Valuation,
    pub dmg: Valuation,
}

impl ByCondition {
    /// [NM, LP, MP, HP, DMG] values, the shape `CardView::by_condition` takes.
    pub fn values(&self) -> [Option<f64>; 5] {
        [
            self.nm.value,
            self.lp.value,
            self.mp.value,
            self.hp.value,
            self.dmg.value,
        ]
    }
}

/// Works out a value for NM, LP, MP, HP and DMG. Per condition, the first that applies:
/// 1. a per-condition source price (JustTCG), named with its source and date;
/// 2. the research range (midpoint, low..high); HP and DMG come from the MP range times
///    `research_derived`;
/// 3. the NM price times the condition multiplier, with its band (NM itself as is).
pub fn valuate_all(
    nm: Option<&NmQuote>,
    research: Option<&ResearchEntry>,
    cond_prices: Option<&StoredConditionPrices>,
    cfg: &ConditionConfig,
) -> ByCondition {
    let one = |c: Condition| valuate_one(c, nm, research, cond_prices, cfg);
    ByCondition {
        nm: one(Condition::NearMint),
        lp: one(Condition::LightlyPlayed),
        mp: one(Condition::ModeratelyPlayed),
        hp: one(Condition::HeavilyPlayed),
        dmg: one(Condition::Damaged),
    }
}

fn valuate_one(
    c: Condition,
    nm: Option<&NmQuote>,
    research: Option<&ResearchEntry>,
    cond_prices: Option<&StoredConditionPrices>,
    cfg: &ConditionConfig,
) -> Valuation {
    // 1. Per-condition source price.
    if let Some(sp) = cond_prices {
        let p = &sp.prices;
        let price = match c {
            Condition::NearMint => p.nm,
            Condition::LightlyPlayed => p.lp,
            Condition::ModeratelyPlayed => p.mp,
            Condition::HeavilyPlayed => p.hp,
            Condition::Damaged => p.dmg,
            Condition::Unchecked => None,
        };
        if let Some(v) = price.filter(|v| v.is_finite() && *v > 0.0) {
            let date = p
                .updated
                .as_deref()
                .and_then(day_of)
                .or_else(|| day_of(&sp.fetched_at));
            let src = source_name(&p.source);
            let basis = match date {
                Some(d) => format!("{src} {}, {d}", c.code()),
                None => format!("{src} {}", c.code()),
            };
            return Valuation::of(v, v, v, basis);
        }
    }
    // 2. Research range.
    if let Some(r) = research {
        let d = &cfg.research_derived;
        let (range, factor, what) = match c {
            Condition::NearMint => (r.nm, 1.0, "NM range".to_string()),
            Condition::LightlyPlayed => (r.lp, 1.0, "LP range".to_string()),
            Condition::ModeratelyPlayed => (r.mp, 1.0, "MP range".to_string()),
            Condition::HeavilyPlayed => {
                (r.mp, d.hp_from_mp, format!("MP range × {}", d.hp_from_mp))
            }
            Condition::Damaged => (r.mp, d.dmg_from_mp, format!("MP range × {}", d.dmg_from_mp)),
            Condition::Unchecked => return Valuation::none(),
        };
        let lo = range[0] * factor;
        let hi = range[1] * factor;
        return Valuation::of(
            (lo + hi) / 2.0,
            lo,
            hi,
            format!("Research {what}, {}", r.date),
        );
    }
    // 3. NM times the multiplier.
    let Some(q) = nm else {
        return Valuation::none();
    };
    match cfg.multiplier(c) {
        None => {
            let basis = match &q.date {
                Some(d) => format!("{} market, {d}", q.source.short()),
                None => format!("{} market", q.source.short()),
            };
            Valuation::of(q.price, q.price, q.price, basis)
        }
        Some(m) => Valuation::of(
            q.price * m.value,
            q.price * m.low,
            q.price * m.high,
            format!("{} × {} (estimate)", q.source.short(), m.value),
        ),
    }
}

/// The valuation used for a copy in `condition`. An unchecked copy is valued as NM (README "Piles":
/// piling rule), with its range running from the HP value up to the NM high.
pub fn value_for(by: &ByCondition, condition: Condition) -> Valuation {
    match condition {
        Condition::NearMint => by.nm.clone(),
        Condition::LightlyPlayed => by.lp.clone(),
        Condition::ModeratelyPlayed => by.mp.clone(),
        Condition::HeavilyPlayed => by.hp.clone(),
        Condition::Damaged => by.dmg.clone(),
        Condition::Unchecked => {
            if by.nm.value.is_none() {
                return Valuation::none();
            }
            Valuation {
                value: by.nm.value,
                low: by.hp.value.or(by.nm.low),
                high: by.nm.high,
                basis: format!("unchecked, priced as NM ({})", by.nm.basis),
            }
        }
    }
}

fn source_name(s: &str) -> String {
    if s.eq_ignore_ascii_case("justtcg") {
        "JustTCG".to_string()
    } else {
        s.to_string()
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

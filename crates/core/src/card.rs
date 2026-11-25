//! Shared types for card data that comes from outside (TCGdex today), and the lookup trait the
//! network crate implements. Core never does network I/O itself: `identity` and `ledger` take a
//! `&mut dyn CardLookup`, and tests pass a fake.

use crate::model::Variant;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which TCGdex language catalogue a card lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Lang {
    En,
    Ja,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
        }
    }
    pub fn parse(s: &str) -> Option<Lang> {
        match s {
            "en" => Some(Lang::En),
            "ja" => Some(Lang::Ja),
            _ => None,
        }
    }
}

/// Which printings TCGdex says exist for a card (its `variants` object).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Printings {
    pub normal: bool,
    pub reverse: bool,
    pub holo: bool,
    pub first_edition: bool,
}

/// One market price line from a source, for one printing bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketPrice {
    /// "tcgplayer" or "cardmarket".
    pub source: String,
    /// The source's own bucket name, unchanged (e.g. "reverse-holofoil", "holofoil", "avg-holo").
    pub bucket: String,
    /// The printing this bucket maps to, when the mapping is certain.
    pub variant: Option<Variant>,
    /// Near Mint market price (TCGplayer "marketPrice"; Cardmarket "trend").
    pub market: Option<f64>,
    pub low: Option<f64>,
    /// ISO 4217: "USD", "EUR", "JPY".
    pub currency: String,
    /// When the source last updated this price (as the source wrote it).
    pub updated: Option<String>,
    /// TCGplayer product id, when the source gives one (JustTCG matches on it).
    pub product_id: Option<u64>,
}

/// A card as the catalogue describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardRecord {
    pub lang: Lang,
    /// Catalogue id, e.g. "dc1-6", "g1-RC7", "xyp-XY150".
    pub id: String,
    pub set_id: String,
    pub set_name: String,
    /// The number printed on the card without the "/total" part, e.g. "6", "RC7", "XY150".
    pub local_id: String,
    pub name: String,
    pub rarity: Option<String>,
    /// "Pokemon", "Trainer" or "Energy".
    pub category: Option<String>,
    /// National Pokédex numbers (first one is used for Pokédex order).
    pub dex_ids: Vec<u32>,
    pub printings: Printings,
    pub prices: Vec<MarketPrice>,
    pub image: Option<String>,
}

/// A short entry from a set listing (enough to match by number or name).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardBrief {
    pub id: String,
    pub local_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LookupError {
    /// Network trouble, a 5xx, a timeout: try again later. Nothing is concluded from it.
    Unavailable(String),
    /// The source asked us to slow down or stop (429, Retry-After).
    RateLimited(String),
    /// The answer could not be read (changed format).
    BadData(String),
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LookupError::Unavailable(e) => write!(f, "card source unavailable: {e}"),
            LookupError::RateLimited(e) => write!(f, "card source asked us to slow down: {e}"),
            LookupError::BadData(e) => write!(f, "card source sent something unreadable: {e}"),
        }
    }
}

impl std::error::Error for LookupError {}

/// Card catalogue access. `Ok(None)` means the source answered "no such card/set" (404).
/// Implementations cache; callers may ask for the same thing many times.
pub trait CardLookup {
    fn card(&mut self, lang: Lang, id: &str) -> Result<Option<CardRecord>, LookupError>;
    fn set_cards(
        &mut self,
        lang: Lang,
        set_id: &str,
    ) -> Result<Option<Vec<CardBrief>>, LookupError>;
}

/// Per-condition prices for one printing from a source that has them (JustTCG), USD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionPrices {
    pub source: String,
    pub variant: Variant,
    pub nm: Option<f64>,
    pub lp: Option<f64>,
    pub mp: Option<f64>,
    pub hp: Option<f64>,
    pub dmg: Option<f64>,
    pub updated: Option<String>,
}

/// What a likely scanner mistake or a data problem looks like. Flags never change anything by
/// themselves; the user decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum FlagCode {
    /// The scanned printing (e.g. reverse holo) isn't one the catalogue lists for this card.
    VariantNotListed,
    /// 1st Edition vs Unlimited can't be told apart from the data; check the stamp.
    FirstEditionCheck,
    /// Base Set / Base Set 2 / Legendary Collection lookalikes.
    BaseSetLookalike,
    /// A promo number vs a numbered set card of the same Pokémon.
    PromoVsSet,
    /// A Radiant Collection number outside the subset's range, or RC in the wrong set.
    RadiantNumber,
    /// A Japanese card (prices usually missing in USD).
    Japanese,
    /// Not a Pokémon card (e.g. Yu-Gi-Oh!): kept, left out of the piles.
    NotPokemon,
    /// Collectr's price and the catalogue's price for this printing differ by more than 3x.
    PriceDisagrees,
    /// The catalogue's name for this number doesn't match the scanned name.
    NameMismatch,
    /// No price for this printing from any source.
    NoPrice,
    /// Matched by name because Collectr's number isn't in the set: the printed number differs.
    NumberDiffers,
    /// Collectr's figure disagrees with the researched NM range (more than 3x outside it).
    ResearchDisagrees,
    /// Collectr's line isn't an ungraded Near Mint figure, so it can't serve as the NM price.
    CollectrNotNm,
}

impl FlagCode {
    pub fn code(self) -> &'static str {
        match self {
            FlagCode::VariantNotListed => "variant-not-listed",
            FlagCode::FirstEditionCheck => "first-edition-check",
            FlagCode::BaseSetLookalike => "base-set-lookalike",
            FlagCode::PromoVsSet => "promo-vs-set",
            FlagCode::RadiantNumber => "radiant-number",
            FlagCode::Japanese => "japanese",
            FlagCode::NotPokemon => "not-pokemon",
            FlagCode::PriceDisagrees => "price-disagrees",
            FlagCode::NameMismatch => "name-mismatch",
            FlagCode::NoPrice => "no-price",
            FlagCode::NumberDiffers => "number-differs",
            FlagCode::ResearchDisagrees => "research-disagrees",
            FlagCode::CollectrNotNm => "collectr-not-nm",
        }
    }
    pub const ALL: [FlagCode; 13] = [
        FlagCode::VariantNotListed,
        FlagCode::FirstEditionCheck,
        FlagCode::BaseSetLookalike,
        FlagCode::PromoVsSet,
        FlagCode::RadiantNumber,
        FlagCode::Japanese,
        FlagCode::NotPokemon,
        FlagCode::PriceDisagrees,
        FlagCode::NameMismatch,
        FlagCode::NoPrice,
        FlagCode::NumberDiffers,
        FlagCode::ResearchDisagrees,
        FlagCode::CollectrNotNm,
    ];
    pub fn parse(s: &str) -> Option<FlagCode> {
        FlagCode::ALL.into_iter().find(|f| f.code() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flag {
    pub code: FlagCode,
    /// One plain sentence for the user: what to look at on the physical card.
    pub message: String,
}

/// The catalogue card one Collectr line was matched to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedCard {
    pub lang: Lang,
    /// Catalogue id, e.g. "dc1-6".
    pub card_id: String,
    pub variant: Variant,
    /// The canonical id: `<lang>:<card_id>/<variant code>` for Japanese, `<card_id>/<variant code>`
    /// for English, e.g. "dc1-6/holo", "ja:SM8b-12/normal".
    pub canonical: String,
    pub name: String,
    pub dex_id: Option<u32>,
    pub rarity: Option<String>,
    pub category: Option<String>,
}

impl ResolvedCard {
    pub fn canonical_for(lang: Lang, card_id: &str, variant: Variant) -> String {
        match lang {
            Lang::En => format!("{card_id}/{}", variant.code()),
            Lang::Ja => format!("ja:{card_id}/{}", variant.code()),
        }
    }
}

/// The outcome of matching one Collectr line (item).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub item_id: i64,
    /// `Some` when resolved; `None` means it is on the Unresolved list.
    pub card: Option<ResolvedCard>,
    /// Why it is unresolved (set when `card` is `None`).
    pub reason: Option<String>,
    pub flags: Vec<Flag>,
}

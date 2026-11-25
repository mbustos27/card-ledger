//! Pile rules (see README.md, "Piles").
//!
//! | Pile | Rule |
//! |---|---|
//! | Toploader | value >= $20 |
//! | Small binder | $2 <= value < $20 |
//! | Big binder (Pokédex order) | value < $2 and foil, rare, promo or better |
//! | Box | everything else: commons, every extra copy of a card, no price yet |
//! | Set aside | not a Pokémon card |
//!
//! Only one copy of each card (its keeper; see `keepers`) takes the pile its value earns.
//!
//! Thresholds come from [`ConditionConfig::piles`].

use crate::model::{Condition, Variant};
use crate::pricing::ConditionConfig;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
pub enum Pile {
    Toploader,
    SmallBinder,
    BigBinder,
    Box,
    SetAside,
}

impl Pile {
    pub const ALL: [Pile; 5] = [
        Pile::Toploader,
        Pile::SmallBinder,
        Pile::BigBinder,
        Pile::Box,
        Pile::SetAside,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Pile::Toploader => "toploader",
            Pile::SmallBinder => "small-binder",
            Pile::BigBinder => "big-binder",
            Pile::Box => "box",
            Pile::SetAside => "set-aside",
        }
    }

    pub fn parse(s: &str) -> Option<Pile> {
        Pile::ALL.into_iter().find(|p| p.code() == s)
    }

    /// How much protection the pile gives; higher is better. Set aside isn't on the scale.
    fn rank(self) -> u8 {
        match self {
            Pile::Toploader => 4,
            Pile::SmallBinder => 3,
            Pile::BigBinder => 2,
            Pile::Box => 1,
            Pile::SetAside => 0,
        }
    }
}

/// One physical card, as the pile rules need it.
#[derive(Debug, Clone, PartialEq)]
pub struct PileInput {
    pub copy_id: i64,
    /// Canonical id ("dc1-6/holo"), or "item:<id>" when unresolved. Copies sharing a group are
    /// copies of the same card.
    pub group: String,
    pub copy_no: u32,
    pub condition: Condition,
    /// The value for its condition (the NM value when unchecked).
    pub value: Option<f64>,
    /// The HP value, used for unchecked cards to see whether wear would move it.
    pub worst_value: Option<f64>,
    pub variant: Variant,
    pub rarity: String,
    pub is_promo: bool,
    pub is_pokemon: bool,
    pub dex_id: Option<u32>,
    pub name: String,
    pub set_name: String,
    pub number: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PileOutcome {
    pub copy_id: i64,
    pub pile: Pile,
    /// One short phrase: "$24.50 ≥ $20", "reverse holo under $2", "Extra copy → box".
    pub reason: String,
    /// Unchecked, and either at its HP value it would go to a cheaper pile, or it's an extra copy
    /// whose NM value would earn a better pile than the (checked) keeper's.
    pub check_condition: bool,
    /// Sort key inside the pile: Pokédex order in the big binder, value descending elsewhere.
    pub order: String,
}

/// Rarities that earn a big-binder slot besides anything with "Rare" in it.
const BINDER_RARITY_WORDS: [&str; 10] = [
    "rare", "promo", "holo", "legend", "amazing", "radiant", "shiny", "ace spec", "prism", "secret",
];

/// Why the card deserves a big-binder slot under $2, or `None` if it doesn't.
fn binder_worthy(c: &PileInput) -> Option<String> {
    if c.variant.is_foil() {
        let what = match c.variant {
            Variant::ReverseHolofoil => "reverse holo",
            Variant::FirstEditionHolofoil => "1st edition holo",
            _ => "holo",
        };
        return Some(what.to_string());
    }
    if c.is_promo {
        return Some("promo".to_string());
    }
    let r = c.rarity.trim().to_lowercase();
    if BINDER_RARITY_WORDS.iter().any(|w| r.contains(w)) {
        return Some(r);
    }
    None
}

fn money(v: f64) -> String {
    format!("${v:.2}")
}

fn whole(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("${v:.0}")
    } else {
        format!("${v:.2}")
    }
}

/// The pile a value alone earns (ignoring which copy is the keeper).
fn value_pile(value: f64, worthy: bool, cfg: &ConditionConfig) -> Pile {
    if value >= cfg.piles.toploader {
        Pile::Toploader
    } else if value >= cfg.piles.small_binder {
        Pile::SmallBinder
    } else if worthy {
        Pile::BigBinder
    } else {
        Pile::Box
    }
}

/// The reason shown for a copy that isn't the keeper of its card.
pub const EXTRA_COPY_REASON: &str = "Extra copy → box";

/// How a copy ranks for "keeper" of its card (lower is better): a copy whose condition has been
/// set beats an unchecked one, then the better condition, then the lowest copy number. Until
/// conditions are set the keeper is copy 1; setting copy 1 to MP and copy 2 to NM makes copy 2
/// the keeper.
fn keeper_rank(c: &PileInput) -> (bool, Condition, u32, i64) {
    (
        c.condition == Condition::Unchecked,
        c.condition,
        c.copy_no,
        c.copy_id,
    )
}

/// The keeper of each group (copies of the same card): every other copy is an extra copy.
pub fn keepers(cards: &[PileInput]) -> HashMap<&str, i64> {
    let mut best: HashMap<&str, &PileInput> = HashMap::new();
    for c in cards.iter().filter(|c| c.is_pokemon) {
        best.entry(c.group.as_str())
            .and_modify(|cur| {
                if keeper_rank(c) < keeper_rank(cur) {
                    *cur = c;
                }
            })
            .or_insert(c);
    }
    best.into_iter().map(|(g, c)| (g, c.copy_id)).collect()
}

/// Sorts every copy into a pile. Outcomes come back in input order.
///
/// Copies of the same card (same group) get one keeper (see [`keeper_rank`]); the keeper goes to
/// the pile its value earns and every other copy goes to the box as trade stock, whatever it is
/// worth. Totals still count every copy's value.
pub fn assign(cards: &[PileInput], cfg: &ConditionConfig) -> Vec<PileOutcome> {
    let small = cfg.piles.small_binder;
    let top = cfg.piles.toploader;
    let keeper = keepers(cards);

    cards
        .iter()
        .map(|c| {
            let worthy = binder_worthy(c);
            let is_keeper = keeper.get(c.group.as_str()) == Some(&c.copy_id);
            let (pile, reason) = if !c.is_pokemon {
                (Pile::SetAside, "not a Pokémon card".to_string())
            } else if !is_keeper {
                (Pile::Box, EXTRA_COPY_REASON.to_string())
            } else {
                match c.value {
                    None => (Pile::Box, "no price yet".to_string()),
                    Some(v) if v >= top => {
                        (Pile::Toploader, format!("{} ≥ {}", money(v), whole(top)))
                    }
                    Some(v) if v >= small => (
                        Pile::SmallBinder,
                        format!("{}, {} to {}", money(v), whole(small), whole(top)),
                    ),
                    Some(_) => match &worthy {
                        Some(why) => (Pile::BigBinder, format!("{why} under {}", whole(small))),
                        None => (Pile::Box, format!("common under {}", whole(small))),
                    },
                }
            };

            let check_condition = if c.is_pokemon && !is_keeper {
                // An unchecked extra might be in better shape than a keeper that has been
                // checked: flag it when its NM value would earn a better pile than the keeper's.
                let keeper_card = keeper
                    .get(c.group.as_str())
                    .and_then(|id| cards.iter().find(|k| k.copy_id == *id));
                c.condition == Condition::Unchecked
                    && keeper_card.is_some_and(|k| {
                        let keeper_pile = k.value.map_or(Pile::Box, |v| {
                            value_pile(v, binder_worthy(k).is_some(), cfg)
                        });
                        k.condition != Condition::Unchecked
                            && c.value.is_some_and(|v| {
                                value_pile(v, worthy.is_some(), cfg).rank() > keeper_pile.rank()
                            })
                    })
            } else {
                c.condition == Condition::Unchecked
                    && pile != Pile::SetAside
                    && c.value.is_some()
                    && c.worst_value
                        .is_some_and(|w| value_pile(w, worthy.is_some(), cfg).rank() < pile.rank())
            };

            let order = match pile {
                Pile::BigBinder => dex_order(c),
                _ => value_order(c),
            };
            PileOutcome {
                copy_id: c.copy_id,
                pile,
                reason,
                check_condition,
                order,
            }
        })
        .collect()
}

/// "0025 pikachu|set|number"; cards with no Pokédex number (trainers, energy) sort after.
fn dex_order(c: &PileInput) -> String {
    let tail = format!(
        "{}|{}|{}",
        c.name.to_lowercase(),
        c.set_name.to_lowercase(),
        c.number
    );
    match c.dex_id {
        Some(d) => format!("{d:04} {tail}"),
        None => format!("~ {tail}"),
    }
}

/// Value descending, then name; no value sorts last.
fn value_order(c: &PileInput) -> String {
    let tail = format!(
        "{}|{}|{}",
        c.name.to_lowercase(),
        c.set_name.to_lowercase(),
        c.number
    );
    match c.value {
        Some(v) if v.is_finite() && v >= 0.0 => {
            let cents = (v * 100.0).round().min(9_999_999_999.0) as u64;
            format!("{:010} {tail}", 9_999_999_999 - cents)
        }
        _ => format!("~ {tail}"),
    }
}

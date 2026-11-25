//! Card identity: maps each Collectr line to a canonical catalogue card and flags likely scanner
//! mistakes. See README.md "What it does".
//!
//! Interface the rest of the ledger uses (keep these signatures):
//! - `SetMap::embedded()` / `SetMap::parse(toml)`: Collectr set name -> TCGdex set id (+ lang).
//! - `normalize_number(raw) -> String`
//! - `resolve(item, &SetMap, &mut dyn CardLookup) -> Result<Resolution, LookupError>`
//!   (an `Err` means "try later", never "unresolved").
//! - `price_flags(item, resolved, nm_usd) -> Vec<Flag>` for the >3x price disagreement check,
//!   called after prices are known.
//!
//! Facts about TCGdex this module relies on (checked 2026-11-24, example ids 2026-11-25):
//! - `localId` padding depends on the set: "15" (WotC to Sun & Moon), "025" (Sword & Shield on,
//!   and every Japanese set), "XY01"/"BW09"/"DP01"/"HGSS01" (promos, two digits minimum).
//!   `data/sets.toml` records the padding per set as `pad`.
//! - The `variants` object is unreliable for holo: EX, full-art and Radiant Collection cards are
//!   listed as `normal` only (dc1-6, g1-RC7), and reverse holos are often missing (ex14-44) while
//!   TCGplayer prices a `reverse-holofoil` bucket. So a printing counts as listed when either the
//!   `variants` object or a TCGplayer price bucket says it exists.

use crate::card::{
    CardLookup, CardRecord, Flag, FlagCode, Lang, LookupError, Resolution, ResolvedCard,
};
use crate::db::Item;
use crate::model::Variant;
use serde::Deserialize;
use std::collections::HashSet;

const EMBEDDED_SETS: &str = include_str!("../../../data/sets.toml");

/// One Collectr set name and where its cards live in TCGdex.
#[derive(Debug, Clone, PartialEq)]
pub struct SetEntry {
    /// Collectr's "Set" column, exactly.
    pub name: String,
    pub lang: Lang,
    /// TCGdex set ids to try, in order (most sets have one; trainer kits have two decks).
    pub set_ids: Vec<String>,
    /// Minimum digits TCGdex writes in this set's `localId` (0 = no padding).
    pub pad: usize,
    /// Collectr's Radiant Collection names: numbers there must start with RC.
    pub radiant: bool,
    /// Highest RC number when the TCGdex set has a Radiant Collection.
    pub rc_max: Option<u32>,
    /// Why TCGdex can't match this set (every line of it is Unresolved).
    pub none: Option<String>,
    /// Not a Pokémon card set.
    pub not_pokemon: bool,
}

impl SetEntry {
    /// The first TCGdex set id, if the set is mapped.
    pub fn set_id(&self) -> Option<&str> {
        self.set_ids.first().map(String::as_str)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    set: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    name: String,
    id: Option<String>,
    #[serde(default)]
    alt: Vec<String>,
    lang: Option<String>,
    #[serde(default)]
    pad: usize,
    #[serde(default)]
    radiant: bool,
    rc_max: Option<u32>,
    none: Option<String>,
    #[serde(default)]
    not_pokemon: bool,
}

/// Collectr set name -> TCGdex set, from `data/sets.toml`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SetMap {
    entries: Vec<SetEntry>,
}

impl SetMap {
    /// The map compiled into the program from `data/sets.toml`.
    pub fn embedded() -> SetMap {
        SetMap::parse(EMBEDDED_SETS).expect("data/sets.toml is invalid")
    }

    /// Reads a sets file. Every entry needs exactly one of `id`, `none` or `not_pokemon`.
    pub fn parse(toml_text: &str) -> Result<SetMap, String> {
        let raw: RawFile = toml::from_str(toml_text).map_err(|e| format!("sets file: {e}"))?;
        let mut seen = HashSet::new();
        let mut entries = Vec::with_capacity(raw.set.len());
        for r in raw.set {
            let name = r.name.trim().to_string();
            if name.is_empty() {
                return Err("sets file: an entry has an empty name".into());
            }
            if !seen.insert(name.clone()) {
                return Err(format!("sets file: \"{name}\" is listed twice"));
            }
            let kinds = usize::from(r.id.is_some())
                + usize::from(r.none.is_some())
                + usize::from(r.not_pokemon);
            if kinds != 1 {
                return Err(format!(
                    "sets file: \"{name}\" needs exactly one of id, none or not_pokemon"
                ));
            }
            if r.id.is_none() && !r.alt.is_empty() {
                return Err(format!("sets file: \"{name}\" has alt without id"));
            }
            let lang = match r.lang.as_deref() {
                None => Lang::En,
                Some(s) => Lang::parse(s)
                    .ok_or_else(|| format!("sets file: \"{name}\" has unknown lang \"{s}\""))?,
            };
            if r.pad > 4 {
                return Err(format!("sets file: \"{name}\" has pad {} (max 4)", r.pad));
            }
            let mut set_ids: Vec<String> = r.id.into_iter().chain(r.alt).collect();
            for id in &mut set_ids {
                *id = id.trim().to_string();
                if id.is_empty() || id.contains(['/', '?', '#', ' ']) {
                    return Err(format!("sets file: \"{name}\" has a bad set id \"{id}\""));
                }
            }
            entries.push(SetEntry {
                name,
                lang,
                set_ids,
                pad: r.pad,
                radiant: r.radiant,
                rc_max: r.rc_max,
                none: r.none.map(|s| s.trim().to_string()),
                not_pokemon: r.not_pokemon,
            });
        }
        Ok(SetMap { entries })
    }

    /// The entry for a Collectr set name (exact, ignoring surrounding spaces).
    pub fn get(&self, collectr_name: &str) -> Option<&SetEntry> {
        let n = collectr_name.trim();
        self.entries.iter().find(|e| e.name == n)
    }

    pub fn entries(&self) -> &[SetEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The comparable form of a card number: "040/087" -> "40", "064" -> "64", "RC09" -> "RC9",
/// "XY150" stays, "28a" stays. Numbers with a hyphen ("LOB-062") are not Pokémon numbers and are
/// returned trimmed but otherwise unchanged. Use `tcgdex_local_id` to get TCGdex's padded form.
pub fn normalize_number(raw: &str) -> String {
    let t = raw.trim().trim_start_matches('#').trim();
    if t.contains('-') {
        return t.to_string();
    }
    let t = t.split('/').next().unwrap_or("").trim();
    let (prefix, digits, suffix) = split_number(t);
    if digits.is_empty() {
        return t.to_string();
    }
    let d = digits.trim_start_matches('0');
    let d = if d.is_empty() { "0" } else { d };
    format!("{}{d}{suffix}", prefix.to_ascii_uppercase())
}

/// TCGdex's `localId` for a normalized number in a set with the given padding
/// ("40", pad 3 -> "040"; "XY1", pad 2 -> "XY01"; "XY150", pad 2 -> "XY150").
pub fn tcgdex_local_id(normalized: &str, pad: usize) -> String {
    let (prefix, digits, suffix) = split_number(normalized);
    if digits.is_empty() || pad == 0 || normalized.contains('-') {
        return normalized.to_string();
    }
    format!("{prefix}{digits:0>pad$}{suffix}")
}

/// Splits "RC05a" into ("RC", "05", "a"). Without digits the whole string is the prefix and the
/// digit and suffix parts are empty.
fn split_number(s: &str) -> (&str, &str, &str) {
    let start = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    (&s[..start], &rest[..end], &rest[end..])
}

/// The comparable form of a card name. Collectr's parentheticals are dropped ("Zubat (47)",
/// "Spinda (Swirl)", "Lotad (Example [Blue])"), "EX"/"-EX"/" ex" all become " ex",
/// accents and the δ/♀/♂ marks are folded ("Pokémon Catcher" = "Pokemon Catcher", a ♂ mark =
/// " M"), brackets are kept as words ("Example [X]" = "Example X") and "LV.X" is dropped (TCGdex
/// leaves it out of the name).
pub fn normalize_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for c in raw.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            'é' | 'è' | 'ê' | 'É' | 'È' => out.push('e'),
            'á' | 'à' | 'â' => out.push('a'),
            'ó' | 'ô' => out.push('o'),
            'í' => out.push('i'),
            'ú' => out.push('u'),
            '♀' => out.push_str(" f "),
            '♂' => out.push_str(" m "),
            '\'' | '’' | '‘' | '.' | 'δ' | '★' => {}
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            '&' => out.push_str(" & "),
            _ => out.push(' '),
        }
    }
    let words: Vec<&str> = out.split_whitespace().filter(|w| *w != "lvx").collect();
    let mut s = words.join(" ");
    // A name ending in "LV X" (space instead of a dot) carries the same mark.
    if let Some(stripped) = s.strip_suffix(" lv x") {
        s = stripped.to_string();
    }
    s
}

/// True when two names are the same card name. Besides exact equality of the normalized forms,
/// TCGdex's form names count as the same ("Shellos East" = "Shellos East Sea Form").
pub fn names_match(scanned: &str, catalogue: &str) -> bool {
    let a = normalize_name(scanned);
    let b = normalize_name(catalogue);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() < b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    long.ends_with(" form")
        && long
            .strip_prefix(short.as_str())
            .is_some_and(|rest| rest.starts_with(' '))
}

const WOTC_SETS: [&str; 16] = [
    "base1", "base2", "base3", "base4", "base5", "base6", "gym1", "gym2", "neo1", "neo2", "neo3",
    "neo4", "lc", "ecard1", "ecard2", "ecard3",
];

const PROMO_SETS: [&str; 11] = [
    "basep", "wp", "np", "dpp", "hgssp", "bwp", "xyp", "smp", "swshp", "svp", "mep",
];

const PROMO_PREFIXES: [&str; 8] = ["XY", "BW", "DP", "HGSS", "SM", "SWSH", "SVP", "NP"];

fn is_promo(set_id: &str, number: &str) -> bool {
    if PROMO_SETS.contains(&set_id) {
        return true;
    }
    // McDonald's collections: "2011bw", "2012bw", "2014xy", ...
    if set_id.len() > 4 && set_id[..4].bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    let (prefix, digits, _) = split_number(number);
    !digits.is_empty() && PROMO_PREFIXES.contains(&prefix)
}

fn flag(code: FlagCode, message: String) -> Flag {
    Flag { code, message }
}

/// True when Collectr's category column means a Pokémon card: "Pokemon", "Pokémon", "pokemon",
/// "Pokemon Japan". A blank category counts as Pokémon (older exports have none; other games
/// always name themselves, e.g. "YuGiOh").
pub fn is_pokemon_category(category: &str) -> bool {
    let c = normalize_name(category).replace(' ', "");
    c.is_empty() || c.starts_with("pokemon")
}

/// True when TCGdex's `category` is a Pokémon-game card: None (not given), "Pokemon", "Trainer"
/// or "Energy". A blank value is no answer, like None. Anything else is not a Pokémon card.
pub fn is_pokemon_catalog_category(category: Option<&str>) -> bool {
    match category.map(str::trim) {
        None | Some("") => true,
        Some(c) => matches!(c, "Pokemon" | "Trainer" | "Energy"),
    }
}

/// True when the Collectr line is a Japanese card: "(JP)" in the product name (any case), or a
/// Japanese category such as "Pokemon Japan". Its English USD price must not be used.
pub fn looks_japanese(item: &Item) -> bool {
    item.product_name.to_ascii_lowercase().contains("(jp)")
        || normalize_name(&item.category).contains("japan")
}

fn japanese_flag() -> Flag {
    flag(
        FlagCode::Japanese,
        "This is a Japanese card, which usually has no USD price: check that the text on the card is Japanese.".into(),
    )
}

fn not_pokemon_flag(item: &Item) -> Flag {
    let what = if item.category.trim().is_empty() || is_pokemon_category(&item.category) {
        item.set_name.trim().to_string()
    } else {
        item.category.trim().to_string()
    };
    flag(
        FlagCode::NotPokemon,
        format!("This is not a Pokémon card ({what}): it stays in the ledger but is left out of the piles."),
    )
}

fn unresolved(item: &Item, reason: String, flags: Vec<Flag>) -> Resolution {
    Resolution {
        item_id: item.id,
        card: None,
        reason: Some(reason),
        flags,
    }
}

/// How a scanned printing relates to what the catalogue lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Normal,
    Holo,
    Reverse,
    FirstEdition,
}

fn group_of(v: Variant) -> Group {
    match v {
        Variant::Normal | Variant::Unlimited => Group::Normal,
        Variant::Holofoil | Variant::UnlimitedHolofoil => Group::Holo,
        Variant::ReverseHolofoil => Group::Reverse,
        Variant::FirstEdition | Variant::FirstEditionHolofoil => Group::FirstEdition,
    }
}

/// The printing groups TCGplayer prices for this card (its buckets are reliable evidence a
/// printing exists; Cardmarket's "-holo" fields are not, they exist for nearly every card).
fn priced_groups(rec: &CardRecord) -> Vec<Group> {
    let mut out = Vec::new();
    for p in rec.prices.iter().filter(|p| p.source == "tcgplayer") {
        let g = match p.variant {
            Some(v) => Some(group_of(v)),
            None => {
                let b = p.bucket.to_ascii_lowercase();
                if b.contains("1st") || b.contains("first") {
                    Some(Group::FirstEdition)
                } else if b.contains("reverse") {
                    Some(Group::Reverse)
                } else if b.contains("holo") {
                    Some(Group::Holo)
                } else if b.contains("normal") || b.contains("unlimited") {
                    Some(Group::Normal)
                } else {
                    None
                }
            }
        };
        if let Some(g) = g {
            if !out.contains(&g) {
                out.push(g);
            }
        }
    }
    out
}

fn variant_flag(item: &Item, rec: &CardRecord) -> Option<Flag> {
    let p = rec.printings;
    let priced = priced_groups(rec);
    let any_printing = p.normal || p.holo || p.reverse || p.first_edition;
    if !any_printing && priced.is_empty() {
        return None; // nothing known about this card's printings
    }
    let has = |g: Group| {
        priced.contains(&g)
            || match g {
                Group::Normal => p.normal,
                Group::Holo => p.holo,
                Group::Reverse => p.reverse,
                Group::FirstEdition => p.first_edition,
            }
    };
    let v = item.variant;
    let msg = match group_of(v) {
        Group::Reverse if !has(Group::Reverse) => format!(
            "Collectr says {v}, but TCGdex lists no reverse holo of this card: check whether the shine is on the art (holo) or on the rest of the card (reverse holo)."
        ),
        Group::Holo if !has(Group::Holo) && has(Group::Reverse) => format!(
            "Collectr says {v}, but TCGdex lists this card only as normal and reverse holo: check whether the shine is on the art or on the rest of the card."
        ),
        // TCGdex marks many holo-only cards (EX, full art, RC) as "normal" only, and TCGplayer
        // prices them as "holofoil" (a holo bucket counts in `has`). Without any TCGplayer price
        // there is no evidence either way, so a normal-only card is only suspicious when
        // TCGplayer prices it and has no holofoil.
        Group::Holo if !has(Group::Holo) && !priced.is_empty() => format!(
            "Collectr says {v}, but TCGdex and TCGplayer list no holo printing of this card: check whether the art shines."
        ),
        Group::Normal if !has(Group::Normal) => format!(
            "Collectr says {v}, but TCGdex lists only foil printings of this card: check whether the card shines."
        ),
        Group::FirstEdition if !has(Group::FirstEdition) => format!(
            "Collectr says {v}, but TCGdex lists no 1st Edition printing of this card: look for the 1st Edition stamp at the lower left of the art."
        ),
        _ => return None,
    };
    Some(flag(FlagCode::VariantNotListed, msg))
}

/// Flags that depend only on the Collectr line and the set (they apply whether or not the card
/// was found).
fn line_flags(item: &Item, entry: &SetEntry, number: &str, set_id: Option<&str>) -> Vec<Flag> {
    let mut flags = Vec::new();
    let v = item.variant;
    let wotc = set_id.is_some_and(|s| WOTC_SETS.contains(&s));
    if wotc
        || matches!(
            v,
            Variant::FirstEdition
                | Variant::FirstEditionHolofoil
                | Variant::Unlimited
                | Variant::UnlimitedHolofoil
        )
    {
        flags.push(flag(
            FlagCode::FirstEditionCheck,
            format!("Collectr says {v}: check for the black 1st Edition stamp at the lower left of the art, since 1st Edition and Unlimited are priced apart."),
        ));
    }
    if let Some(total) = match set_id {
        Some("base1") => Some(102),
        Some("base4") => Some(130),
        Some("lc") => Some(110),
        _ => None,
    } {
        flags.push(flag(
            FlagCode::BaseSetLookalike,
            format!("Base Set, Base Set 2 and Legendary Collection reuse the same art: check that the number on the card ends in /{total}."),
        ));
    }
    if let Some(s) = set_id {
        if is_promo(s, number) {
            flags.push(flag(
                FlagCode::PromoVsSet,
                format!("This is listed as a promo ({number}): check that the card shows the promo number and symbol, not a regular set number."),
            ));
        }
    }
    let (prefix, digits, _) = split_number(number);
    if prefix == "RC" {
        let n: u32 = digits.parse().unwrap_or(0);
        match entry.rc_max {
            None => flags.push(flag(
                FlagCode::RadiantNumber,
                format!("{} has no Radiant Collection, so {number} is probably misread: check the number on the card.", entry.name),
            )),
            Some(max) if n == 0 || n > max => flags.push(flag(
                FlagCode::RadiantNumber,
                format!("{number} is outside this Radiant Collection (RC1 to RC{max}): check the number on the card."),
            )),
            _ => {}
        }
    } else if entry.radiant {
        flags.push(flag(
            FlagCode::RadiantNumber,
            format!("Collectr lists this in a Radiant Collection, but its number {number} has no RC: check whether the number on the card starts with RC."),
        ));
    }
    if entry.lang == Lang::Ja || looks_japanese(item) {
        flags.push(japanese_flag());
    }
    flags
}

/// Matches one Collectr line to a TCGdex card. `Err` means the catalogue couldn't be asked
/// (try later); a card that can't be matched is `Ok` with `card: None` and a reason.
pub fn resolve(
    item: &Item,
    sets: &SetMap,
    lookup: &mut dyn CardLookup,
) -> Result<Resolution, LookupError> {
    let entry = sets.get(&item.set_name);
    if !is_pokemon_category(&item.category) || entry.is_some_and(|e| e.not_pokemon) {
        let mut flags = vec![not_pokemon_flag(item)];
        if looks_japanese(item) {
            flags.push(japanese_flag());
        }
        return Ok(unresolved(item, "not a Pokémon card".into(), flags));
    }
    let Some(entry) = entry else {
        let flags = if looks_japanese(item) {
            vec![japanese_flag()]
        } else {
            vec![]
        };
        return Ok(unresolved(
            item,
            format!(
                "the set \"{}\" isn't in data/sets.toml yet",
                item.set_name.trim()
            ),
            flags,
        ));
    };
    let number = normalize_number(&item.card_number);
    if let Some(why) = &entry.none {
        let flags = line_flags(item, entry, &number, None);
        return Ok(unresolved(item, why.clone(), flags));
    }
    let lang = entry.lang;
    let several = entry.set_ids.len() > 1;

    // 1. The id built from the number, in each candidate set.
    let mut found: Option<CardRecord> = None;
    let mut first_miss_name: Option<CardRecord> = None;
    if !number.is_empty() {
        for set_id in &entry.set_ids {
            let id = format!("{set_id}-{}", tcgdex_local_id(&number, entry.pad));
            if let Some(rec) = lookup.card(lang, &id)? {
                if !several || names_match(&item.product_name, &rec.name) {
                    found = Some(rec);
                    break;
                }
                first_miss_name.get_or_insert(rec);
            }
        }
    }

    // 2. The set listings: by number (any padding), then by name.
    let mut missing_sets = Vec::new();
    let mut ambiguous: Option<String> = None;
    let mut by_name_only = false;
    if found.is_none() {
        'sets: for set_id in &entry.set_ids {
            let Some(cards) = lookup.set_cards(lang, set_id)? else {
                missing_sets.push(set_id.clone());
                continue;
            };
            let by_number = cards
                .iter()
                .find(|c| !number.is_empty() && normalize_number(&c.local_id) == number);
            if let Some(brief) = by_number {
                if !several || names_match(&item.product_name, &brief.name) {
                    if let Some(rec) = lookup.card(lang, &brief.id)? {
                        found = Some(rec);
                        break 'sets;
                    }
                }
            }
            let by_name: Vec<_> = cards
                .iter()
                .filter(|c| names_match(&item.product_name, &c.name))
                .collect();
            match by_name.as_slice() {
                [one] => {
                    if let Some(rec) = lookup.card(lang, &one.id)? {
                        found = Some(rec);
                        by_name_only = true;
                        break 'sets;
                    }
                }
                [] => {}
                many => {
                    ambiguous.get_or_insert_with(|| {
                        let ids: Vec<&str> = many.iter().map(|c| c.id.as_str()).collect();
                        format!(
                            "several TCGdex cards are named {} and none has number {}: {}",
                            item.product_name.trim(),
                            item.card_number.trim(),
                            ids.join(", ")
                        )
                    });
                }
            }
        }
    }
    // A trainer kit line whose name matched in neither deck: take the first deck's card by number
    // and let the name flag say so.
    let found = found.or(first_miss_name);

    let set_id_for_flags = found.as_ref().map(|r| r.set_id.as_str()).or(entry.set_id());
    let mut flags = line_flags(item, entry, &number, set_id_for_flags);

    let Some(rec) = found else {
        let reason = if let Some(a) = ambiguous {
            a
        } else if missing_sets.len() == entry.set_ids.len() {
            format!("TCGdex has no set {}", missing_sets.join(" or "))
        } else {
            format!(
                "TCGdex has no card numbered {} or named {} in {}",
                item.card_number.trim(),
                item.product_name.trim(),
                entry.set_ids.join(" or ")
            )
        };
        return Ok(unresolved(item, reason, flags));
    };

    if !is_pokemon_catalog_category(rec.category.as_deref()) {
        flags.push(not_pokemon_flag(item));
        return Ok(unresolved(item, "not a Pokémon card".into(), flags));
    }
    if by_name_only && !number.is_empty() && normalize_number(&rec.local_id) != number {
        let said = item.card_number.trim().trim_start_matches('#').trim();
        flags.push(flag(
            FlagCode::NumberDiffers,
            format!(
                "Collectr says #{said} but TCGdex lists this card as #{}; check the number printed on the card.",
                rec.local_id
            ),
        ));
    }
    if let Some(f) = variant_flag(item, &rec) {
        flags.push(f);
    }
    if lang == Lang::En && !names_match(&item.product_name, &rec.name) {
        flags.push(flag(
            FlagCode::NameMismatch,
            format!(
                "TCGdex has {} as number {} in this set, not {}: check the name and number on the card.",
                rec.name,
                rec.local_id,
                item.product_name.trim()
            ),
        ));
    }
    flags.sort_by_key(|f| f.code);
    flags.dedup_by_key(|f| f.code);

    let card = ResolvedCard {
        lang: rec.lang,
        canonical: ResolvedCard::canonical_for(rec.lang, &rec.id, item.variant),
        card_id: rec.id,
        variant: item.variant,
        name: rec.name,
        dex_id: rec.dex_ids.first().copied(),
        rarity: rec.rarity,
        category: rec.category,
    };
    Ok(Resolution {
        item_id: item.id,
        card: Some(card),
        reason: None,
        flags,
    })
}

/// Price checks, run once prices are known. `nm_usd` is the catalogue's NM price in USD for this
/// printing (None when no source has one). Collectr's own figure is `item.collectr_price`.
pub fn price_flags(item: &Item, resolved: Option<&ResolvedCard>, nm_usd: Option<f64>) -> Vec<Flag> {
    let collectr = item.collectr_price.filter(|p| p.is_finite() && *p > 0.0);
    let catalogue = nm_usd.filter(|p| p.is_finite() && *p > 0.0);
    let what = resolved.map_or_else(|| item.label(), |r| r.canonical.clone());
    match (collectr, catalogue) {
        (Some(c), Some(n)) if c > n * 3.0 || n > c * 3.0 => vec![flag(
            FlagCode::PriceDisagrees,
            format!(
                "Collectr prices {what} at ${c:.2} but TCGdex says ${n:.2} NM: a gap this big usually means the wrong printing or card, so check both on the card."
            ),
        )],
        (None, None) => vec![flag(
            FlagCode::NoPrice,
            format!("No source has a price for {what}: check the printing on the card and price it by hand if it matters."),
        )],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_pad() {
        assert_eq!(split_number("RC05a"), ("RC", "05", "a"));
        assert_eq!(tcgdex_local_id("25", 3), "025");
        assert_eq!(tcgdex_local_id("XY1", 2), "XY01");
        assert_eq!(tcgdex_local_id("XY150", 2), "XY150");
        assert_eq!(tcgdex_local_id("28a", 0), "28a");
        assert_eq!(tcgdex_local_id("LOB-062", 3), "LOB-062");
    }

    #[test]
    fn embedded_parses() {
        let m = SetMap::embedded();
        assert!(m.len() > 60);
    }
}

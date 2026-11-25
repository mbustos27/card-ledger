//! The integrated view: identity + prices + condition values + piles, end to end on the sample
//! export with a fake catalogue (no network).

use card_ledger_core::card::{
    CardBrief, CardLookup, CardRecord, FlagCode, Lang, LookupError, MarketPrice, Printings,
};
use card_ledger_core::db::Db;
use card_ledger_core::import::parse;
use card_ledger_core::ledger::{self, CardView, LedgerView};
use card_ledger_core::model::{Condition, Variant};
use card_ledger_core::pricing::Research;
use std::collections::HashMap;

/// Made-up research ranges for tests (the build may embed an empty or a private file).
fn research() -> &'static Research {
    static R: std::sync::OnceLock<Research> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Research::parse(include_str!("fixtures/research-sample.json")).expect("fixture")
    })
}

fn view(db: &Db) -> rusqlite::Result<LedgerView> {
    ledger::build_view_with(db, research())
}

const SAMPLE: &str = include_str!("fixtures/sample.csv");

struct Fake {
    cards: HashMap<String, CardRecord>,
    fail: bool,
    /// Card ids that answer "unavailable" (a passing outage for just those lookups).
    down: Vec<String>,
}

fn rec(id: &str, name: &str, dex: Option<u32>, p: Printings) -> CardRecord {
    let (set, local) = id.split_once('-').unwrap();
    CardRecord {
        lang: Lang::En,
        id: id.into(),
        set_id: set.into(),
        set_name: set.into(),
        local_id: local.into(),
        name: name.into(),
        rarity: Some("Rare".into()),
        category: Some("Pokemon".into()),
        dex_ids: dex.into_iter().collect(),
        printings: p,
        prices: vec![],
        image: None,
    }
}

impl Fake {
    fn new() -> Fake {
        let both = Printings {
            normal: true,
            reverse: true,
            holo: false,
            first_edition: false,
        };
        let holo = Printings {
            normal: false,
            reverse: false,
            holo: true,
            first_edition: false,
        };
        let mut cards = HashMap::new();
        for r in [
            rec("xy7-47", "Zubat", Some(41), both),
            rec("bw11-9", "Oddish", Some(43), both),
            rec("bw11-RC24", "Skyla", None, holo),
            rec("xy1-25", "Staryu", Some(120), both),
        ] {
            cards.insert(r.id.clone(), r);
        }
        Fake {
            cards,
            fail: false,
            down: vec![],
        }
    }
}

impl CardLookup for Fake {
    fn card(&mut self, _lang: Lang, id: &str) -> Result<Option<CardRecord>, LookupError> {
        if self.fail || self.down.iter().any(|d| d == id) {
            return Err(LookupError::Unavailable("fake outage".into()));
        }
        Ok(self.cards.get(id).cloned())
    }
    fn set_cards(
        &mut self,
        _lang: Lang,
        set_id: &str,
    ) -> Result<Option<Vec<CardBrief>>, LookupError> {
        if self.fail {
            return Err(LookupError::Unavailable("fake outage".into()));
        }
        let list: Vec<CardBrief> = self
            .cards
            .values()
            .filter(|c| c.set_id == set_id)
            .map(|c| CardBrief {
                id: c.id.clone(),
                local_id: c.local_id.clone(),
                name: c.name.clone(),
            })
            .collect();
        Ok(if list.is_empty() { None } else { Some(list) })
    }
}

fn tcg(bucket: &str, variant: Variant, market: f64) -> MarketPrice {
    MarketPrice {
        source: "tcgplayer".into(),
        bucket: bucket.into(),
        variant: Some(variant),
        market: Some(market),
        low: None,
        currency: "USD".into(),
        updated: Some("2026-11-24T22:16:27Z".into()),
        product_id: Some(1),
    }
}

fn setup() -> Db {
    let mut db = Db::open_in_memory().unwrap();
    db.import(&parse(SAMPLE.as_bytes()).unwrap()).unwrap();
    let rep = ledger::resolve_all(&db, &mut Fake::new(), false, &mut |_, _, _| {}).unwrap();
    assert!(rep.stopped.is_none());
    db
}

fn card<'a>(v: &'a LedgerView, name: &str, variant: &str) -> Vec<&'a CardView> {
    v.cards
        .iter()
        .filter(|c| c.product_name.starts_with(name) && c.variant == variant)
        .collect()
}

#[test]
fn every_card_is_resolved_or_listed_unresolved() {
    let db = setup();
    let v = view(&db).unwrap();
    assert_eq!(v.total_cards, 14);
    for c in &v.cards {
        assert!(
            c.canonical.is_some() ^ c.unresolved_reason.is_some(),
            "{} must be exactly one of resolved/unresolved",
            c.product_name
        );
    }
    let staryu = card(&v, "Staryu", "Reverse Holofoil");
    assert_eq!(staryu[0].canonical.as_deref(), Some("xy1-25/reverse"));
    let skyla = card(&v, "Skyla", "Holofoil");
    assert_eq!(skyla.len(), 3);
    assert_eq!(skyla[0].canonical.as_deref(), Some("bw11-RC24/holo"));
    // Yu-Gi-Oh: unresolved, flagged, set aside, and not in the value total.
    let yugioh_card = card(&v, "Mystical Elf", "1st Edition");
    assert_eq!(yugioh_card[0].pile, "set-aside");
    assert!(yugioh_card[0]
        .flags
        .iter()
        .any(|f| f.code == FlagCode::NotPokemon));
    assert!(yugioh_card[0].unresolved_reason.is_some());
    // Nothing is treated as checked.
    assert_eq!(v.unchecked, 14);
}

#[test]
fn prices_drive_value_by_condition_and_pile() {
    let db = setup();
    // Staryu reverse: TCGplayer NM $30 (the sample says $2.00: a >3x disagreement).
    db.save_quotes(
        Lang::En,
        "xy1-25",
        &[
            tcg("reverse-holofoil", Variant::ReverseHolofoil, 30.0),
            tcg("normal", Variant::Normal, 0.50),
        ],
        "2026-11-24T23:00:00Z",
    )
    .unwrap();
    let v = view(&db).unwrap();
    let rev = card(&v, "Staryu", "Reverse Holofoil")[0].clone();
    let normal = card(&v, "Staryu", "Normal")[0].clone();
    // Unchecked: piled by NM value, with check-condition because HP (~$7) is a cheaper pile.
    assert_eq!(rev.value, Some(30.0));
    assert_eq!(rev.pile, "toploader");
    assert!(rev.check_condition);
    assert!(rev.flags.iter().any(|f| f.code == FlagCode::PriceDisagrees));
    assert!(rev.nm_label.as_deref().unwrap().contains("TCGplayer"));
    // The normal printing never borrows the reverse price.
    assert_eq!(normal.nm_price, Some(0.50));
    assert_eq!(v.prices_fetched_at.as_deref(), Some("2026-11-24T23:00:00Z"));

    // Set it to MP: value drops to NM x 0.36 = $10.80, small binder, no check flag.
    db.set_condition(rev.copy_id, Condition::ModeratelyPlayed)
        .unwrap();
    let v2 = view(&db).unwrap();
    let rev2 = card(&v2, "Staryu", "Reverse Holofoil")[0].clone();
    assert_eq!(rev2.condition, "MP");
    assert_eq!(rev2.value, Some(10.8));
    assert_eq!(rev2.pile, "small-binder");
    assert!(!rev2.check_condition);
    assert_eq!(rev2.by_condition[2], Some(10.8));
    assert!(v2.total_value < v.total_value);
    let tl = v2.piles.iter().find(|p| p.pile == "toploader").unwrap();
    assert_eq!(tl.cards, 0);
}

#[test]
fn extra_copies_of_cheap_holos_go_to_the_box() {
    let db = setup();
    db.save_quotes(
        Lang::En,
        "bw11-RC24",
        &[tcg("holofoil", Variant::Holofoil, 1.20)],
        "2026-11-24T23:00:00Z",
    )
    .unwrap();
    let v = view(&db).unwrap();
    let skyla = card(&v, "Skyla", "Holofoil");
    let piles: Vec<&str> = skyla.iter().map(|c| c.pile.as_str()).collect();
    assert!(
        piles.contains(&"big-binder") && piles.contains(&"box"),
        "{piles:?}"
    );
}

#[test]
fn a_catalogue_outage_stops_matching_without_marking_cards_unresolved() {
    let mut db = Db::open_in_memory().unwrap();
    db.import(&parse(SAMPLE.as_bytes()).unwrap()).unwrap();
    let mut fake = Fake::new();
    fake.fail = true;
    let rep = ledger::resolve_all(&db, &mut fake, false, &mut |_, _, _| {}).unwrap();
    assert!(rep.stopped.is_some());
    // Lines before the outage that need no lookup (none in this order) may be saved; nothing
    // that needed the catalogue was written as unresolved.
    for r in db.resolutions().unwrap() {
        assert!(
            r.card.is_some() || r.reason.as_deref().is_some_and(|x| !x.contains("outage")),
            "{r:?}"
        );
    }
    let targets = ledger::price_targets(&db).unwrap();
    assert!(targets.is_empty());
}

#[test]
fn rematching_keeps_conditions_and_ticks() {
    let db = setup();
    let v = view(&db).unwrap();
    let id = card(&v, "Oddish", "Reverse Holofoil")[0].copy_id;
    db.set_condition(id, Condition::LightlyPlayed).unwrap();
    db.set_pulled(id, true).unwrap();
    ledger::resolve_all(&db, &mut Fake::new(), false, &mut |_, _, _| {}).unwrap();
    let v2 = view(&db).unwrap();
    let c = v2.cards.iter().find(|c| c.copy_id == id).unwrap();
    assert_eq!(c.condition, "LP");
    assert!(c.pulled);
}

// ---- re-imports, totals, Collectr figures, flags, outages, promos ----

fn import_text(db: &mut Db, text: &str) -> card_ledger_core::db::ImportReport {
    db.import(&parse(text.as_bytes()).unwrap()).unwrap()
}

#[test]
fn a_line_corrected_in_collectr_is_not_counted_twice_and_can_carry_over() {
    let db0 = setup();
    let mut db = db0;
    let v = view(&db).unwrap();
    let staryu = card(&v, "Staryu", "Normal")[0].clone();
    db.set_condition(staryu.copy_id, Condition::LightlyPlayed)
        .unwrap();
    db.set_pulled(staryu.copy_id, true).unwrap();

    // The user corrects Staryu Normal -> Holofoil in Collectr and re-imports.
    let fixed = SAMPLE.replace(
        "XY Base Set,Staryu,25,Common,Normal,",
        "XY Base Set,Staryu,25,Common,Holofoil,",
    );
    let r = import_text(&mut db, &fixed);
    assert_eq!(r.corrections.len(), 1, "{:?}", r.corrections);
    let corr = r.corrections[0].clone();
    assert!(corr.old_label.contains("Normal") && corr.new_label.contains("Holofoil"));

    let v2 = view(&db).unwrap();
    assert_eq!(
        v2.total_cards, 14,
        "the old line's copy is not counted again"
    );
    assert_eq!(v2.not_in_export, 1);
    let old = v2
        .cards
        .iter()
        .find(|c| c.copy_id == staryu.copy_id)
        .unwrap();
    assert_eq!(old.pile, ledger::NOT_IN_EXPORT);
    assert!(!old.in_last_export);
    let tile = v2
        .piles
        .iter()
        .find(|p| p.pile == ledger::NOT_IN_EXPORT)
        .unwrap();
    assert_eq!(tile.cards, 1);

    // Nothing moved by itself; carry-over moves the recorded state once.
    let new = card(&v2, "Staryu", "Holofoil")[0].clone();
    assert_eq!(new.condition, "unchecked");
    assert_eq!(db.carry_over(corr.old_item, corr.new_item).unwrap(), 1);
    let v3 = view(&db).unwrap();
    let new3 = v3.cards.iter().find(|c| c.copy_id == new.copy_id).unwrap();
    assert_eq!((new3.condition.as_str(), new3.pulled), ("LP", true));
    let old3 = v3
        .cards
        .iter()
        .find(|c| c.copy_id == staryu.copy_id)
        .unwrap();
    assert_eq!((old3.condition.as_str(), old3.pulled), ("unchecked", false));
    // A second carry-over finds nothing to move; wrong directions are refused.
    assert_eq!(db.carry_over(corr.old_item, corr.new_item).unwrap(), 0);
    assert!(db.carry_over(corr.new_item, corr.old_item).is_err());
    assert!(db.carry_over(999, corr.new_item).is_err());
}

#[test]
fn totals_count_unchecked_cards_at_lp_and_leave_set_aside_out() {
    let db = setup();
    db.save_quotes(
        Lang::En,
        "xy1-25",
        &[tcg("reverse-holofoil", Variant::ReverseHolofoil, 30.0)],
        "2026-11-24T23:00:00Z",
    )
    .unwrap();
    let v = view(&db).unwrap();
    let rev = card(&v, "Staryu", "Reverse Holofoil")[0].clone();
    assert_eq!(rev.value, Some(30.0), "piled at NM");
    assert_eq!(rev.value_counted, rev.by_condition[1], "counted at LP");
    let tl = v.piles.iter().find(|p| p.pile == "toploader").unwrap();
    assert_eq!(tl.value, rev.by_condition[1].unwrap());
    assert_eq!(tl.value_if_nm, 30.0);
    let pile_sum: f64 = v
        .piles
        .iter()
        .filter(|p| p.pile != "set-aside")
        .map(|p| p.value)
        .sum();
    assert!(
        (pile_sum - v.total_value_unchecked_lp).abs() < 0.011,
        "{pile_sum} vs {}",
        v.total_value_unchecked_lp
    );
    // Mystical Elf ($7.00 in the sample) is set aside and not in any total.
    let yugioh_card = card(&v, "Mystical Elf", "1st Edition")[0].clone();
    let aside = v.piles.iter().find(|p| p.pile == "set-aside").unwrap();
    assert_eq!(aside.cards, 1);
    let with_yugioh: f64 = v
        .cards
        .iter()
        .filter(|c| c.in_last_export)
        .map(|c| c.collectr_price.unwrap_or(0.0))
        .sum();
    assert!((with_yugioh - v.collectr_total - yugioh_card.collectr_price.unwrap()).abs() < 0.011);
}

#[test]
fn collectr_figures_that_are_not_raw_nm_are_never_used_as_nm() {
    let mut db = Db::open_in_memory().unwrap();
    let text = SAMPLE
        .replace(
            "XY Base Set,Staryu,25,Common,Normal,Ungraded,Near Mint",
            "XY Base Set,Staryu,25,Common,Normal,Ungraded,Lightly Played",
        )
        .replace(
            "Oddish,9,Common,Reverse Holofoil,Ungraded,Near Mint",
            "Oddish,9,Common,Reverse Holofoil,PSA 10,Near Mint",
        );
    import_text(&mut db, &text);
    ledger::resolve_all(&db, &mut Fake::new(), false, &mut |_, _, _| {}).unwrap();
    let v = view(&db).unwrap();
    for name in ["Staryu", "Oddish"] {
        let c = v
            .cards
            .iter()
            .find(|c| c.product_name == name && (c.variant == "Normal" || name == "Oddish"))
            .unwrap();
        assert_eq!(
            c.nm_price, None,
            "{name}: no NM price from a non-NM Collectr line"
        );
        assert!(
            c.flags.iter().any(|f| f.code == FlagCode::CollectrNotNm),
            "{name}"
        );
        assert_eq!(c.value, None);
    }
}

#[test]
fn a_japanese_line_matched_to_an_english_card_never_takes_its_price() {
    let mut db = Db::open_in_memory().unwrap();
    let text = SAMPLE.replace(
        "XY Base Set,Staryu,25,Common,Reverse Holofoil",
        "XY Base Set,Staryu (JP),25,Common,Reverse Holofoil",
    );
    import_text(&mut db, &text);
    ledger::resolve_all(&db, &mut Fake::new(), false, &mut |_, _, _| {}).unwrap();
    db.save_quotes(
        Lang::En,
        "xy1-25",
        &[tcg("reverse-holofoil", Variant::ReverseHolofoil, 30.0)],
        "2026-11-24T23:00:00Z",
    )
    .unwrap();
    let v = view(&db).unwrap();
    let jp = v
        .cards
        .iter()
        .find(|c| c.product_name == "Staryu (JP)")
        .unwrap();
    assert!(
        jp.canonical.is_some(),
        "the JP line matches the English card by number"
    );
    assert_ne!(
        jp.nm_price,
        Some(30.0),
        "English TCGplayer price used for a JP card"
    );
    assert!(jp.flags.iter().any(|f| f.code == FlagCode::Japanese));
}

#[test]
fn collectr_far_from_the_research_range_is_flagged() {
    // Zorua BW13: research NM $5-15 (made-up fixture); a line at $75.00.
    let mut db = Db::open_in_memory().unwrap();
    let header = SAMPLE.lines().next().unwrap();
    let line = "Main,Pokemon,Black and White Promos,Zorua (BW13),BW13,Promo,Holofoil,Ungraded,Near Mint,0.0000,1,75.00,0,false,2026-11-25,";
    import_text(&mut db, &format!("{header}\n{line}\n"));
    let v = view(&db).unwrap();
    let z = &v.cards[0];
    assert!(
        z.research.is_some(),
        "research entry for Zorua BW13 matched"
    );
    assert!(
        z.flags
            .iter()
            .any(|f| f.code == FlagCode::ResearchDisagrees),
        "{:?}",
        z.flags
    );
}

#[test]
fn a_brief_outage_skips_lines_and_the_next_run_matches_them() {
    let db = {
        let mut db = Db::open_in_memory().unwrap();
        db.import(&parse(SAMPLE.as_bytes()).unwrap()).unwrap();
        db
    };
    let mut fake = Fake::new();
    fake.down = vec!["xy7-47".into()];
    let rep = ledger::resolve_all(&db, &mut fake, false, &mut |_, _, _| {}).unwrap();
    assert!(rep.stopped.is_none());
    assert_eq!(rep.skipped, 1);
    let v = view(&db).unwrap();
    let zubat = card(&v, "Zubat", "Normal")[0].clone();
    assert!(zubat
        .unresolved_reason
        .as_deref()
        .unwrap()
        .contains("not matched yet"));
    // Next run, source back: only the missing line is looked up and matched.
    let rep2 = ledger::resolve_all(&db, &mut Fake::new(), true, &mut |_, _, _| {}).unwrap();
    assert_eq!(rep2.skipped, 0);
    let v2 = view(&db).unwrap();
    assert_eq!(
        card(&v2, "Zubat", "Normal")[0].canonical.as_deref(),
        Some("xy7-47/normal")
    );
}

#[test]
fn outages_count_over_the_run_not_in_a_row() {
    let mut db = Db::open_in_memory().unwrap();
    db.import(&parse(SAMPLE.as_bytes()).unwrap()).unwrap();
    // Three different lines down, with working lines between them: the third stops the run.
    let mut fake = Fake::new();
    fake.down = vec!["xy7-47".into(), "bw11-9".into(), "xy1-25".into()];
    let rep = ledger::resolve_all(&db, &mut fake, false, &mut |_, _, _| {}).unwrap();
    assert_eq!(rep.skipped, 2);
    assert!(matches!(rep.stopped, Some(LookupError::Unavailable(_))));
}

#[test]
fn promos_are_recognised() {
    use card_ledger_core::card::{Resolution, ResolvedCard};
    let mut db = Db::open_in_memory().unwrap();
    db.import(&parse(SAMPLE.as_bytes()).unwrap()).unwrap();
    let item = db
        .items()
        .unwrap()
        .into_iter()
        .find(|i| i.product_name == "Staryu")
        .unwrap();
    let res = |id: &str| Resolution {
        item_id: item.id,
        card: Some(ResolvedCard {
            lang: Lang::En,
            card_id: id.into(),
            variant: Variant::Normal,
            canonical: format!("{id}/normal"),
            name: "x".into(),
            dex_id: None,
            rarity: None,
            category: None,
        }),
        reason: None,
        flags: vec![],
    };
    for id in ["xyp-XY62", "bwp-BW13", "mcd14-3", "SM-P-012", "SVP-001"] {
        assert!(ledger::is_promo(&item, Some(&res(id))), "{id}");
    }
    for id in ["xy1-25", "sm1-10", "g1-RC3"] {
        assert!(!ledger::is_promo(&item, Some(&res(id))), "{id}");
    }
    let mut promo_item = item.clone();
    promo_item.rarity = "Promo".into();
    assert!(ledger::is_promo(&promo_item, None));
}

// ---- extra copies: one keeper per card, the rest to the box ----

fn oddish_copies(v: &LedgerView) -> Vec<CardView> {
    let mut c: Vec<CardView> = v
        .cards
        .iter()
        .filter(|c| c.product_name == "Oddish")
        .cloned()
        .collect();
    c.sort_by_key(|c| c.copy_no);
    c
}

#[test]
fn extra_copies_go_to_the_box_and_the_keeper_follows_condition() {
    let db = setup();
    db.save_quotes(
        Lang::En,
        "bw11-9",
        &[tcg("reverse-holofoil", Variant::ReverseHolofoil, 5.0)],
        "2026-11-24T23:00:00Z",
    )
    .unwrap();
    let v = view(&db).unwrap();
    let o = oddish_copies(&v);
    assert_eq!(o.len(), 4);
    // Unchecked: copy 1 keeps its pile ($5 -> small binder), copies 2-4 are trade stock.
    assert_eq!(o[0].pile, "small-binder");
    for c in &o[1..] {
        assert_eq!(
            (c.pile.as_str(), c.pile_reason.as_str()),
            ("box", card_ledger_core::piles::EXTRA_COPY_REASON)
        );
    }
    // Totals still count every copy: the box tile holds the three extras' value.
    let box_tile = v.piles.iter().find(|p| p.pile == "box").unwrap();
    let extras: f64 = o[1..].iter().map(|c| c.value_counted.unwrap()).sum();
    assert!(
        box_tile.value + 1e-9 >= extras,
        "box {} vs extras {extras}",
        box_tile.value
    );
    let sum: f64 = v
        .piles
        .iter()
        .filter(|p| p.pile != "set-aside" && p.pile != ledger::NOT_IN_EXPORT)
        .map(|p| p.value)
        .sum();
    assert!((sum - v.total_value_unchecked_lp).abs() < 0.011);

    // Copy 1 is MP, copy 2 NM: copy 2 becomes the keeper.
    db.set_condition(o[0].copy_id, Condition::ModeratelyPlayed)
        .unwrap();
    db.set_condition(o[1].copy_id, Condition::NearMint).unwrap();
    let o2 = oddish_copies(&view(&db).unwrap());
    assert_eq!(o2[1].pile, "small-binder");
    assert_eq!(o2[0].pile, "box");
    assert_eq!(
        o2[0].pile_reason,
        card_ledger_core::piles::EXTRA_COPY_REASON
    );

    // Re-importing the same export keeps the choice (conditions and copies are untouched).
    let mut db = db;
    db.import(&parse(SAMPLE.as_bytes()).unwrap()).unwrap();
    let o3 = oddish_copies(&view(&db).unwrap());
    assert_eq!(
        o3.iter().map(|c| c.pile.as_str()).collect::<Vec<_>>(),
        vec!["box", "small-binder", "box", "box"]
    );
}

#[test]
fn a_graded_slab_is_never_an_extra_copy_of_the_raw_card() {
    let mut db = Db::open_in_memory().unwrap();
    let header = SAMPLE.lines().next().unwrap();
    let raw = "Main,Pokemon,Legendary Treasures,Oddish,9,Common,Reverse Holofoil,Ungraded,Near Mint,0.0000,2,3.00,0,false,2026-01-15,; ";
    let slab = "Main,Pokemon,Legendary Treasures,Oddish,9,Common,Reverse Holofoil,PSA 10,Near Mint,0.0000,1,40.00,0,false,2026-01-15,";
    db.import(&parse(format!("{header}\n{raw}\n{slab}\n").as_bytes()).unwrap())
        .unwrap();
    ledger::resolve_all(&db, &mut Fake::new(), false, &mut |_, _, _| {}).unwrap();
    db.save_quotes(
        Lang::En,
        "bw11-9",
        &[tcg("reverse-holofoil", Variant::ReverseHolofoil, 5.0)],
        "2026-11-24T23:00:00Z",
    )
    .unwrap();
    let v = view(&db).unwrap();
    let extra = card_ledger_core::piles::EXTRA_COPY_REASON;
    let raws: Vec<&CardView> = v
        .cards
        .iter()
        .filter(|c| c.product_name == "Oddish" && c.copies_of_item == 2)
        .collect();
    let slab = v.cards.iter().find(|c| c.copies_of_item == 1).unwrap();
    assert_ne!(slab.pile_reason, extra, "the PSA 10 is its own card");
    assert_eq!(
        raws.iter().filter(|c| c.pile_reason == extra).count(),
        1,
        "one of the two raw copies is an extra"
    );
}

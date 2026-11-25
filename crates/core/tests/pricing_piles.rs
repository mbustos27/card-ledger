//! Condition pricing and pile rules.

use card_ledger_core::card::{ConditionPrices, MarketPrice};
use card_ledger_core::db::{StoredConditionPrices, StoredQuote};
use card_ledger_core::model::{Condition, Variant};
use card_ledger_core::piles::{assign, Pile, PileInput};
use card_ledger_core::pricing::{
    norm_number, pick_nm_quote, valuate_all, value_for, ConditionConfig, NmQuote, NmSource,
    Research,
};

/// Made-up research ranges for tests (the build may embed an empty or a private file).
fn research() -> &'static Research {
    static R: std::sync::OnceLock<Research> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Research::parse(include_str!("fixtures/research-sample.json")).expect("fixture")
    })
}

fn cfg() -> &'static ConditionConfig {
    ConditionConfig::embedded()
}

fn quote(
    source: &str,
    variant: Option<Variant>,
    market: f64,
    currency: &str,
    updated: &str,
) -> StoredQuote {
    StoredQuote {
        quote: MarketPrice {
            source: source.into(),
            bucket: variant
                .map(|v| v.code().to_string())
                .unwrap_or_else(|| "mystery".into()),
            variant,
            market: Some(market),
            low: None,
            currency: currency.into(),
            updated: Some(updated.into()),
            product_id: None,
        },
        fetched_at: "2026-11-24T10:00:00Z".into(),
    }
}

fn nm(price: f64) -> NmQuote {
    NmQuote {
        price,
        label: "TCGdex · TCGplayer NM market · 2026-11-24".into(),
        date: Some("2026-11-24".into()),
        source: NmSource::Tcgplayer,
    }
}

fn justtcg(
    nm: Option<f64>,
    lp: Option<f64>,
    mp: Option<f64>,
    hp: Option<f64>,
    dmg: Option<f64>,
) -> StoredConditionPrices {
    StoredConditionPrices {
        prices: ConditionPrices {
            source: "justtcg".into(),
            variant: Variant::Holofoil,
            nm,
            lp,
            mp,
            hp,
            dmg,
            updated: Some("2026-11-23T08:00:00Z".into()),
        },
        fetched_at: "2026-11-24T09:00:00Z".into(),
    }
}

// ---- config ----

#[test]
fn embedded_config_has_the_research_figures() {
    let c = cfg();
    assert_eq!(c.multipliers.lp.value, 0.54);
    assert_eq!(c.multipliers.mp.value, 0.36);
    assert_eq!(c.multipliers.hp.value, 0.24);
    assert_eq!(c.multipliers.dmg.value, 0.15);
    assert_eq!((c.multipliers.lp.low, c.multipliers.lp.high), (0.45, 0.65));
    assert_eq!((c.multipliers.mp.low, c.multipliers.mp.high), (0.28, 0.42));
    assert_eq!(c.piles.toploader, 20.0);
    assert_eq!(c.piles.small_binder, 2.0);
}

#[test]
fn config_rejects_a_band_that_misses_its_value() {
    let bad = include_str!("../../../data/conditions.toml").replace("low = 0.45", "low = 0.60");
    assert!(ConditionConfig::parse(&bad).is_err());
    let swapped = include_str!("../../../data/conditions.toml")
        .replace("small_binder = 2.0", "small_binder = 25.0");
    assert!(ConditionConfig::parse(&swapped).is_err());
}

// ---- research matching ----

#[test]
fn research_fixture_matches_names_and_numbers() {
    let r = research();
    // The made-up research fixture (11 entries).
    assert!(r.entries.len() >= 11, "{} entries", r.entries.len());
    let h = Variant::Holofoil;
    let g = r
        .find("Team Aqua's Kyogre EX", "6", "Double Crisis", h)
        .expect("Kyogre");
    assert_eq!(g.nm, [10.0, 20.0]);
    assert_eq!(g.date, "2026-11-24");
    let c = r
        .find("Yveltal EX", "XY62", "XY Promos", h)
        .expect("Yveltal EX");
    assert_eq!(c.lp, [50.0, 100.0]);
    // TCGdex-style spelling of the same card.
    assert_eq!(
        r.find("Yveltal-EX", "xy62", "XY Promos", h)
            .map(|e| &e.name),
        Some(&c.name)
    );
    // Same number, different card.
    assert!(r
        .find("Team Aqua's Seviper", "6", "Double Crisis", h)
        .is_none());
    // Two RC1s in the research: name decides.
    assert_eq!(
        r.find("Tepig", "RC1", "Generations: Radiant Collection", h)
            .unwrap()
            .nm,
        [30.0, 40.0]
    );
    assert_eq!(
        r.find(
            "Snivy",
            "RC1",
            "Legendary Treasures: Radiant Collections",
            h
        )
        .unwrap()
        .name,
        "Snivy"
    );
}

#[test]
fn research_never_prices_another_printing_or_set() {
    let r = research();
    // Researched as 1st Edition; an Unlimited copy is a different, cheaper card.
    assert!(r
        .find("Omanyte", "52", "Fossil", Variant::FirstEdition)
        .is_some());
    assert!(r
        .find("Omanyte", "52", "Fossil", Variant::Unlimited)
        .is_none());
    assert!(r.find("Omanyte", "52", "Fossil", Variant::Normal).is_none());
    // Same name and number in another set.
    assert!(r
        .find(
            "Team Aqua's Kyogre EX",
            "6",
            "Team Magma vs Team Aqua",
            Variant::Holofoil
        )
        .is_none());
    // Holo range never prices a reverse holo.
    assert!(r
        .find("Yveltal EX", "XY62", "XY Promos", Variant::ReverseHolofoil)
        .is_none());
    // Every entry carries its set and printing.
    assert!(r
        .entries
        .iter()
        .all(|e| e.set.is_some() && e.variant.is_some()));
}

#[test]
fn research_matches_collectr_numbers_and_bracketed_names() {
    let r = research();
    let (h, rev) = (Variant::Holofoil, Variant::ReverseHolofoil);
    assert_eq!(norm_number("040/087"), "40");
    assert_eq!(norm_number("RC09"), "RC9");
    assert_eq!(norm_number("LOB-062"), "LOB-062");
    assert_eq!(
        r.find("Squirtle (JP)", "007/087", "20th Anniversary", h)
            .unwrap()
            .name,
        "Squirtle (JP)"
    );
    assert_eq!(
        r.find("Squirtle (JP)", "7", "20th Anniversary", h)
            .unwrap()
            .name,
        "Squirtle (JP)"
    );
    // An English Squirtle never takes the Japanese entry.
    assert!(r.find("Squirtle", "7", "20th Anniversary", h).is_none());
    assert_eq!(
        r.find("Bidoof (71)", "71/99", "Platinum Arceus", rev)
            .unwrap()
            .name,
        "Bidoof (71)"
    );
    assert_eq!(
        r.find("Bidoof", "71", "Platinum Arceus", rev).unwrap().name,
        "Bidoof (71)"
    );
    assert_eq!(
        r.find("Xerneas EX", "145/146", "XY Base Set", h)
            .unwrap()
            .name,
        "Xerneas EX (145 Full Art)"
    );
    assert_eq!(
        r.find("Psyduck", "220/198", "Scarlet & Violet Base Set", h)
            .unwrap()
            .nm,
        [80.0, 90.0]
    );
    // A bracketed Collectr name with no research entry.
    assert!(r
        .find("Zubat (33)", "33", "Ancient Origins", Variant::Normal)
        .is_none());
    assert!(r
        .find(
            "Mystical Elf",
            "LOB-062",
            "The Legend of Blue Eyes White Dragon",
            Variant::FirstEdition
        )
        .is_some());
}

#[test]
fn bracketed_names_match_their_entry() {
    let json = r#"[{"n":"Zubat (33)","no":"33","nm":[1,2],"lp":[0.5,1],"mp":[0.3,0.6],
        "conf":"low","check":"","grade":"no","note":""}]"#;
    let r = Research::parse(json).unwrap();
    assert!(r
        .find("Zubat (33)", "33", "Ancient Origins", Variant::Normal)
        .is_some());
    assert!(r
        .find("Zubat (33)", "033/100", "Ancient Origins", Variant::Normal)
        .is_some());
    assert!(r
        .find("Zubat", "33", "Ancient Origins", Variant::Normal)
        .is_some());
    assert!(r
        .find("Zubat", "34", "Ancient Origins", Variant::Normal)
        .is_none());
}

#[test]
fn research_rejects_a_reversed_range() {
    let json = r#"[{"n":"X","no":"1","nm":[5,2],"lp":[1,1],"mp":[1,1],"conf":"","check":"","grade":"","note":""}]"#;
    assert!(Research::parse(json).is_err());
}

// ---- NM quote ----

#[test]
fn nm_quote_uses_only_the_matching_printing() {
    let quotes = vec![
        quote(
            "tcgplayer",
            Some(Variant::ReverseHolofoil),
            9.99,
            "USD",
            "2026-11-24",
        ),
        quote(
            "tcgplayer",
            Some(Variant::Normal),
            0.31,
            "USD",
            "2026-11-24T05:00:00Z",
        ),
        quote(
            "cardmarket",
            Some(Variant::Normal),
            0.80,
            "EUR",
            "2026-11-24",
        ),
    ];
    let q = pick_nm_quote(
        Variant::Normal,
        &quotes,
        Some((5.0, Some("2026-11-25".into()))),
    )
    .unwrap();
    assert_eq!(q.price, 0.31);
    assert_eq!(q.label, "TCGdex · TCGplayer NM market · 2026-11-24");
    assert_eq!(q.source, NmSource::Tcgplayer);
}

#[test]
fn nm_quote_never_borrows_another_printing_or_euros() {
    // Only a reverse bucket, a bucket of unknown printing and a EUR figure exist for a Normal card.
    let quotes = vec![
        quote(
            "tcgplayer",
            Some(Variant::ReverseHolofoil),
            9.99,
            "USD",
            "2026-11-24",
        ),
        quote("tcgplayer", None, 7.77, "USD", "2026-11-24"),
        quote(
            "cardmarket",
            Some(Variant::Normal),
            0.80,
            "EUR",
            "2026-11-24",
        ),
        quote(
            "tcgplayer",
            Some(Variant::Normal),
            0.55,
            "EUR",
            "2026-11-24",
        ),
    ];
    let q = pick_nm_quote(
        Variant::Normal,
        &quotes,
        Some((0.42, Some("2026-11-25".into()))),
    )
    .unwrap();
    assert_eq!(q.price, 0.42);
    assert_eq!(q.label, "Collectr (NM) · 2026-11-25");
    assert_eq!(q.source, NmSource::Collectr);
    assert!(pick_nm_quote(Variant::Normal, &quotes, None).is_none());
    assert!(pick_nm_quote(Variant::Normal, &quotes, Some((0.0, None))).is_none());
}

#[test]
fn nm_quote_takes_the_newest_bucket() {
    let mut old = quote(
        "tcgplayer",
        Some(Variant::Holofoil),
        10.0,
        "USD",
        "2026-11-01",
    );
    old.fetched_at = "2026-11-01T00:00:00Z".into();
    let new = quote(
        "tcgplayer",
        Some(Variant::Holofoil),
        12.0,
        "USD",
        "2026-11-24",
    );
    let q = pick_nm_quote(Variant::Holofoil, &[new, old], None).unwrap();
    assert_eq!(q.price, 12.0);
}

// ---- valuation priorities ----

#[test]
fn per_condition_price_wins_for_every_condition() {
    let r = research().find("Yveltal EX", "XY62", "XY Promos", Variant::Holofoil);
    let sp = justtcg(Some(190.0), Some(95.0), Some(60.0), Some(40.0), Some(25.0));
    let by = valuate_all(Some(&nm(200.00)), r, Some(&sp), cfg());
    assert_eq!(
        by.values(),
        [Some(190.0), Some(95.0), Some(60.0), Some(40.0), Some(25.0)]
    );
    assert_eq!(by.lp.basis, "JustTCG LP, 2026-11-23");
    assert_eq!(by.dmg.basis, "JustTCG DMG, 2026-11-23");
    assert_eq!((by.mp.low, by.mp.high), (Some(60.0), Some(60.0)));
}

#[test]
fn research_range_is_next_with_hp_and_dmg_from_mp() {
    let r = research().find("Yveltal EX", "XY62", "XY Promos", Variant::Holofoil);
    // JustTCG has only an NM price: the other conditions fall to the research.
    let sp = justtcg(Some(190.0), None, None, None, None);
    let by = valuate_all(Some(&nm(200.00)), r, Some(&sp), cfg());
    assert_eq!(by.nm.value, Some(190.0));
    assert_eq!(by.lp.value, Some(75.0));
    assert_eq!((by.lp.low, by.lp.high), (Some(50.0), Some(100.0)));
    assert_eq!(by.lp.basis, "Research LP range, 2026-11-24");
    assert_eq!(by.mp.value, Some(50.0));
    // HP = MP 40..60 × 0.67 = 26.80..40.20, midpoint 33.50.
    assert_eq!(
        (by.hp.value, by.hp.low, by.hp.high),
        (Some(33.5), Some(26.8), Some(40.2))
    );
    assert_eq!(by.hp.basis, "Research MP range × 0.67, 2026-11-24");
    // DMG = 40..60 × 0.42 = 16.80..25.20.
    assert_eq!(
        (by.dmg.value, by.dmg.low, by.dmg.high),
        (Some(21.0), Some(16.8), Some(25.2))
    );
    // Without JustTCG the NM is the research NM midpoint.
    let by = valuate_all(Some(&nm(200.00)), r, None, cfg());
    assert_eq!(by.nm.value, Some(150.0));
    assert_eq!(by.nm.basis, "Research NM range, 2026-11-24");
}

#[test]
fn multipliers_are_last() {
    let by = valuate_all(Some(&nm(10.0)), None, None, cfg());
    assert_eq!(
        by.values(),
        [Some(10.0), Some(5.4), Some(3.6), Some(2.4), Some(1.5)]
    );
    assert_eq!(by.nm.basis, "TCGplayer NM market, 2026-11-24");
    assert_eq!(by.lp.basis, "TCGplayer NM × 0.54 (estimate)");
    assert_eq!((by.lp.low, by.lp.high), (Some(4.5), Some(6.5)));
    assert_eq!((by.mp.low, by.mp.high), (Some(2.8), Some(4.2)));
    assert_eq!((by.hp.low, by.hp.high), (Some(1.9), Some(2.8)));
    assert_eq!((by.dmg.low, by.dmg.high), (Some(1.2), Some(1.8)));
    let collectr = pick_nm_quote(
        Variant::Normal,
        &[],
        Some((10.0, Some("2026-11-25".into()))),
    )
    .unwrap();
    let by = valuate_all(Some(&collectr), None, None, cfg());
    assert_eq!(by.mp.basis, "Collectr NM × 0.36 (estimate)");
    assert_eq!(by.nm.basis, "Collectr NM market, 2026-11-25");
}

#[test]
fn no_nm_and_no_research_means_no_value() {
    let by = valuate_all(None, None, None, cfg());
    assert_eq!(by.values(), [None; 5]);
    assert_eq!(by.lp.basis, "no price yet");
    let u = value_for(&by, Condition::Unchecked);
    assert_eq!(u.value, None);
    // Research alone still prices every condition.
    let r = research().find(
        "Team Aqua's Kyogre EX",
        "6",
        "Double Crisis",
        Variant::Holofoil,
    );
    let by = valuate_all(None, r, None, cfg());
    assert!(by.values().iter().all(|v| v.is_some()));
}

#[test]
fn unchecked_is_nm_with_a_range_down_to_hp() {
    let by = valuate_all(Some(&nm(10.0)), None, None, cfg());
    let u = value_for(&by, Condition::Unchecked);
    assert_eq!(
        (u.value, u.low, u.high),
        (Some(10.0), Some(2.4), Some(10.0))
    );
    assert!(u.basis.starts_with("unchecked, priced as NM"));
    assert_eq!(value_for(&by, Condition::LightlyPlayed).value, Some(5.4));
    let r = research().find("Yveltal EX", "XY62", "XY Promos", Variant::Holofoil);
    let by = valuate_all(None, r, None, cfg());
    let u = value_for(&by, Condition::Unchecked);
    assert_eq!(
        (u.value, u.low, u.high),
        (Some(150.0), Some(33.5), Some(200.0))
    );
}

// ---- piles ----

fn card(copy_id: i64, value: Option<f64>) -> PileInput {
    PileInput {
        copy_id,
        group: format!("set-{copy_id}/normal"),
        copy_no: 1,
        condition: Condition::NearMint,
        value,
        worst_value: None,
        variant: Variant::Normal,
        rarity: "Common".into(),
        is_promo: false,
        is_pokemon: true,
        dex_id: Some(1),
        name: format!("Card {copy_id}"),
        set_name: "Set".into(),
        number: copy_id.to_string(),
    }
}

fn one(c: PileInput) -> (Pile, String, bool) {
    let o = assign(&[c], cfg()).remove(0);
    (o.pile, o.reason, o.check_condition)
}

#[test]
fn pile_boundaries_at_exactly_two_and_twenty() {
    assert_eq!(one(card(1, Some(20.0))).0, Pile::Toploader);
    assert_eq!(one(card(1, Some(19.99))).0, Pile::SmallBinder);
    assert_eq!(one(card(1, Some(2.0))).0, Pile::SmallBinder);
    assert_eq!(one(card(1, Some(1.99))).0, Pile::Box);
    let mut holo = card(1, Some(1.99));
    holo.variant = Variant::Holofoil;
    assert_eq!(one(holo.clone()).0, Pile::BigBinder);
    holo.value = Some(2.0);
    assert_eq!(one(holo).0, Pile::SmallBinder);
    assert_eq!(one(card(1, Some(24.5))).1, "$24.50 ≥ $20");
}

#[test]
fn big_binder_needs_foil_rare_or_promo() {
    let mut rev = card(1, Some(0.4));
    rev.variant = Variant::ReverseHolofoil;
    assert_eq!(
        one(rev),
        (Pile::BigBinder, "reverse holo under $2".into(), false)
    );
    let mut rare = card(1, Some(0.4));
    rare.rarity = "Rare".into();
    assert_eq!(one(rare).0, Pile::BigBinder);
    let mut ultra = card(1, Some(1.0));
    ultra.rarity = "Illustration Rare".into();
    assert_eq!(one(ultra).0, Pile::BigBinder);
    let mut promo = card(1, Some(0.5));
    promo.is_promo = true;
    assert_eq!(one(promo).1, "promo under $2");
    let mut uncommon = card(1, Some(0.5));
    uncommon.rarity = "Uncommon".into();
    assert_eq!(one(uncommon).0, Pile::Box);
    let mut first_ed = card(1, Some(0.5));
    first_ed.variant = Variant::FirstEdition;
    assert_eq!(one(first_ed).0, Pile::Box);
}

#[test]
fn no_price_goes_to_the_box() {
    let mut holo = card(1, None);
    holo.variant = Variant::Holofoil;
    assert_eq!(one(holo), (Pile::Box, "no price yet".into(), false));
}

#[test]
fn extra_copies_go_to_the_box_and_the_best_checked_copy_is_kept() {
    let mk = |id: i64, copy_no: u32, cond: Condition, value: f64| {
        let mut c = card(id, Some(value));
        c.group = "xy1-5/reverse".into();
        c.variant = Variant::ReverseHolofoil;
        c.copy_no = copy_no;
        c.condition = cond;
        c
    };
    let extra = card_ledger_core::piles::EXTRA_COPY_REASON;
    // Three copies, all unchecked: copy 1 is the keeper, copies 2 and 3 are extras.
    let cards = vec![
        mk(10, 1, Condition::Unchecked, 0.5),
        mk(11, 2, Condition::Unchecked, 0.5),
        mk(12, 3, Condition::Unchecked, 0.5),
    ];
    let out = assign(&cards, cfg());
    assert_eq!(
        out.iter().map(|o| o.copy_id).collect::<Vec<_>>(),
        vec![10, 11, 12]
    );
    assert_eq!(out[0].pile, Pile::BigBinder);
    assert_eq!((out[1].pile, out[1].reason.as_str()), (Pile::Box, extra));
    assert_eq!((out[2].pile, out[2].reason.as_str()), (Pile::Box, extra));
    // Copy 1 marked MP, copy 2 NM: the NM copy becomes the keeper.
    let cards = vec![
        mk(10, 1, Condition::ModeratelyPlayed, 0.5),
        mk(11, 2, Condition::NearMint, 0.5),
        mk(12, 3, Condition::Unchecked, 0.5),
    ];
    let out = assign(&cards, cfg());
    assert_eq!(
        (out[0].pile, out[1].pile, out[2].pile),
        (Pile::Box, Pile::BigBinder, Pile::Box)
    );
    // A checked copy beats an unchecked one, even a worse condition.
    let cards = vec![
        mk(20, 1, Condition::Unchecked, 0.5),
        mk(21, 2, Condition::Damaged, 0.5),
    ];
    let out = assign(&cards, cfg());
    assert_eq!((out[0].pile, out[1].pile), (Pile::Box, Pile::BigBinder));
    // Same condition: lowest copy number wins.
    let cards = vec![
        mk(30, 2, Condition::NearMint, 0.5),
        mk(31, 1, Condition::NearMint, 0.5),
    ];
    let out = assign(&cards, cfg());
    assert_eq!((out[0].pile, out[1].pile), (Pile::Box, Pile::BigBinder));
    // Extra copies go to the box whatever they are worth; the keeper takes its value pile.
    let cards = vec![
        mk(40, 1, Condition::NearMint, 3.0),
        mk(41, 2, Condition::NearMint, 25.0),
    ];
    let out = assign(&cards, cfg());
    assert_eq!((out[0].pile, out[1].pile), (Pile::SmallBinder, Pile::Box));
    assert_eq!(out[1].reason, extra);
    assert!(
        !out[1].check_condition,
        "a checked extra has nothing left to check"
    );
    // An unchecked extra whose NM value beats the checked keeper's pile asks to be checked...
    let mut maybe_better = mk(43, 2, Condition::Unchecked, 25.0);
    maybe_better.worst_value = Some(5.0);
    let out = assign(
        &[mk(42, 1, Condition::Damaged, 3.0), maybe_better.clone()],
        cfg(),
    );
    assert_eq!((out[0].pile, out[1].pile), (Pile::SmallBinder, Pile::Box));
    assert!(
        out[1].check_condition,
        "it may be in better shape than the keeper"
    );
    // ...but not when it couldn't earn a better pile, or while the keeper is unchecked too.
    let mut cheap = maybe_better.clone();
    cheap.value = Some(3.5);
    let out = assign(&[mk(44, 1, Condition::Damaged, 3.0), cheap], cfg());
    assert!(!out[1].check_condition);
    let out = assign(&[mk(45, 1, Condition::Unchecked, 3.0), maybe_better], cfg());
    assert!(!out[1].check_condition);
    // Different groups each have their own keeper.
    let mut other = mk(51, 1, Condition::NearMint, 0.5);
    other.group = "xy1-6/reverse".into();
    let out = assign(&[mk(50, 1, Condition::NearMint, 0.5), other], cfg());
    assert!(out.iter().all(|o| o.pile == Pile::BigBinder));
    // A non-Pokémon copy in the group never takes the keeper slot.
    let mut not_pokemon = mk(60, 1, Condition::NearMint, 0.5);
    not_pokemon.is_pokemon = false;
    let out = assign(&[not_pokemon, mk(61, 2, Condition::Unchecked, 0.5)], cfg());
    assert_eq!(
        (out[0].pile, out[1].pile),
        (Pile::SetAside, Pile::BigBinder)
    );
    // Two export lines of the same card, each with a copy 1: the lower copy id is the keeper,
    // whatever the input order.
    let out = assign(
        &[
            mk(71, 1, Condition::Unchecked, 0.5),
            mk(70, 1, Condition::Unchecked, 0.5),
        ],
        cfg(),
    );
    assert_eq!((out[0].pile, out[1].pile), (Pile::Box, Pile::BigBinder));
}

#[test]
fn unchecked_cards_flag_a_cheaper_worst_case() {
    let mut c = card(1, Some(25.0));
    c.condition = Condition::Unchecked;
    c.worst_value = Some(6.0);
    assert!(one(c.clone()).2, "toploader at NM, small binder at HP");
    c.worst_value = Some(20.0);
    assert!(!one(c.clone()).2, "still a toploader at HP");
    c.value = Some(3.0);
    c.worst_value = Some(0.72);
    assert!(one(c.clone()).2, "small binder at NM, box at HP");
    c.variant = Variant::Holofoil;
    c.value = Some(1.5);
    c.worst_value = Some(0.36);
    assert!(!one(c.clone()).2, "big binder either way");
    // A checked card is never flagged.
    c.condition = Condition::LightlyPlayed;
    c.value = Some(25.0);
    c.worst_value = Some(6.0);
    assert!(!one(c).2);
}

#[test]
fn big_binder_is_in_pokedex_order_with_trainers_last() {
    let mk = |id: i64, dex: Option<u32>, name: &str| {
        let mut c = card(id, Some(0.5));
        c.variant = Variant::Holofoil;
        c.dex_id = dex;
        c.name = name.into();
        c
    };
    let cards = vec![
        mk(1, None, "Professor's Research"),
        mk(2, Some(151), "Mew"),
        mk(3, Some(39), "Jigglypuff"),
        mk(4, None, "Blend Energy"),
        mk(5, Some(7), "Squirtle"),
    ];
    let out = assign(&cards, cfg());
    let mut sorted = out.clone();
    sorted.sort_by(|a, b| a.order.cmp(&b.order));
    assert_eq!(
        sorted.iter().map(|o| o.copy_id).collect::<Vec<_>>(),
        vec![5, 3, 2, 4, 1]
    );
}

#[test]
fn other_piles_sort_by_value_descending() {
    let cards = vec![
        card(1, Some(3.0)),
        card(2, Some(15.5)),
        card(3, Some(2.0)),
        card(4, Some(9.99)),
    ];
    let mut out = assign(&cards, cfg());
    out.sort_by(|a, b| a.order.cmp(&b.order));
    assert_eq!(
        out.iter().map(|o| o.copy_id).collect::<Vec<_>>(),
        vec![2, 4, 1, 3]
    );
}

#[test]
fn yugioh_card_is_set_aside() {
    let r = research()
        .find(
            "Mystical Elf",
            "LOB-062",
            "The Legend of Blue Eyes White Dragon",
            Variant::FirstEdition,
        )
        .unwrap();
    let by = valuate_all(None, Some(r), None, cfg());
    let mut c = card(1, by.nm.value);
    c.is_pokemon = false;
    c.name = "Mystical Elf".into();
    c.number = "LOB-062".into();
    c.variant = Variant::FirstEdition;
    c.condition = Condition::Unchecked;
    c.worst_value = by.hp.value;
    assert_eq!(one(c), (Pile::SetAside, "not a Pokémon card".into(), false));
}

#[test]
fn pile_codes() {
    let codes: Vec<_> = Pile::ALL.iter().map(|p| p.code()).collect();
    assert_eq!(
        codes,
        [
            "toploader",
            "small-binder",
            "big-binder",
            "box",
            "set-aside"
        ]
    );
    for p in Pile::ALL {
        assert_eq!(Pile::parse(p.code()), Some(p));
    }
}

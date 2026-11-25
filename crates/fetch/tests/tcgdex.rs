mod common;

use card_ledger_core::card::{CardLookup, Lang, LookupError, Printings};
use card_ledger_core::model::Variant;
use card_ledger_fetch::tcgdex::{
    self, parse_card, parse_prices, parse_set_cards, path_segment, tcgplayer_bucket_variant,
};
use card_ledger_fetch::{FakeTransport, FetchError, Response, Tcgdex};
use common::*;

fn price<'a>(
    c: &'a card_ledger_core::card::CardRecord,
    source: &str,
    bucket: &str,
) -> &'a card_ledger_core::card::MarketPrice {
    c.prices
        .iter()
        .find(|p| p.source == source && p.bucket == bucket)
        .unwrap_or_else(|| panic!("no {source}/{bucket} in {:?}", c.prices))
}

#[test]
fn parses_dc1_6() {
    let c = parse_card(Lang::En, &fixture("en_cards_dc1-6.json")).unwrap();
    assert_eq!(
        (
            c.id.as_str(),
            c.set_id.as_str(),
            c.set_name.as_str(),
            c.local_id.as_str()
        ),
        ("dc1-6", "dc1", "Double Crisis", "6")
    );
    assert_eq!(c.name, "Team Aqua's Kyogre EX");
    assert_eq!(c.rarity.as_deref(), Some("Ultra Rare"));
    assert_eq!(c.category.as_deref(), Some("Pokemon"));
    assert_eq!(c.dex_ids, vec![382]);
    assert_eq!(
        c.printings,
        Printings {
            normal: true,
            ..Default::default()
        }
    );
    assert_eq!(
        c.image.as_deref(),
        Some("https://assets.tcgdex.net/en/xy/dc1/6")
    );
    assert_eq!(c.prices.len(), 3);
    let h = price(&c, "tcgplayer", "holofoil");
    assert_eq!(h.variant, Some(Variant::Holofoil));
    assert_eq!(
        (h.market, h.low, h.product_id),
        (Some(10.0), Some(5.0), Some(100006))
    );
    assert_eq!(h.currency, "USD");
    assert_eq!(h.updated.as_deref(), Some("2026-11-24T22:16:27.722Z"));
    let t = price(&c, "cardmarket", "trend");
    assert_eq!(
        (t.market, t.low, t.variant, t.currency.as_str()),
        (Some(8.0), Some(2.0), None, "EUR")
    );
    assert_eq!(t.updated.as_deref(), Some("2026-11-24T22:16:19.112Z"));
    assert_eq!(
        t.product_id, None,
        "Cardmarket's idProduct is not a TCGplayer id"
    );
    let th = price(&c, "cardmarket", "trend-holo");
    assert_eq!((th.market, th.low), (Some(4.0), None));
}

#[test]
fn parses_xy1_121_two_buckets_one_product() {
    let c = parse_card(Lang::En, &fixture("en_cards_xy1-121.json")).unwrap();
    assert_eq!(c.category.as_deref(), Some("Trainer"));
    assert!(c.dex_ids.is_empty());
    let n = price(&c, "tcgplayer", "normal");
    let r = price(&c, "tcgplayer", "reverse-holofoil");
    assert_eq!(
        (n.variant, n.market, n.product_id),
        (Some(Variant::Normal), Some(1.0), Some(87658))
    );
    assert_eq!(
        (r.variant, r.market, r.low, r.product_id),
        (
            Some(Variant::ReverseHolofoil),
            Some(2.5),
            Some(1.5),
            Some(87658)
        )
    );
    assert_eq!(price(&c, "cardmarket", "trend-holo").low, Some(1.0));
}

#[test]
fn parses_g1_rc7_without_tcgplayer() {
    let c = parse_card(Lang::En, &fixture("en_cards_g1-RC7.json")).unwrap();
    assert_eq!(
        (c.local_id.as_str(), c.set_name.as_str()),
        ("RC7", "Generations")
    );
    assert_eq!(c.dex_ids, vec![361]);
    assert!(
        c.prices.iter().all(|p| p.source == "cardmarket"),
        "{:?}",
        c.prices
    );
    assert_eq!(c.prices.len(), 2);
}

#[test]
fn parses_xyp_promo_and_base1_first_edition() {
    let p = parse_card(Lang::En, &fixture("en_cards_xyp-XY150.json")).unwrap();
    assert_eq!(
        (p.local_id.as_str(), p.rarity.as_deref()),
        ("XY150", Some("Promo"))
    );
    assert_eq!(price(&p, "tcgplayer", "holofoil").product_id, Some(100150));

    let b = parse_card(Lang::En, &fixture("en_cards_base1-24.json")).unwrap();
    assert_eq!(
        b.printings,
        Printings {
            normal: true,
            first_edition: true,
            ..Default::default()
        }
    );
    assert_eq!(b.dex_ids, vec![5]);
    let n = price(&b, "tcgplayer", "normal");
    assert_eq!((n.market, n.product_id), (Some(2.5), Some(100024)));
    assert_eq!(
        b.prices.iter().filter(|p| p.source == "tcgplayer").count(),
        1
    );
}

#[test]
fn parses_japanese_card() {
    let c = parse_card(Lang::Ja, &fixture("ja_cards_SV2a-007.json")).unwrap();
    assert_eq!(c.lang, Lang::Ja);
    assert_eq!(
        (c.id.as_str(), c.local_id.as_str(), c.name.as_str()),
        ("SV2a-007", "007", "ゼニガメ")
    );
    assert_eq!(c.set_name, "ポケモンカード151");
    assert_eq!(
        c.printings,
        Printings {
            normal: true,
            reverse: true,
            ..Default::default()
        }
    );
    assert_eq!(c.dex_ids, vec![7]);
    assert!(c
        .prices
        .iter()
        .all(|p| p.source == "cardmarket" && p.currency == "EUR"));
    assert_eq!(price(&c, "cardmarket", "trend").market, Some(0.5));
}

#[test]
fn parses_set_listing() {
    let cards = parse_set_cards(&fixture("en_sets_dc1.json")).unwrap();
    assert_eq!(cards.len(), 3);
    assert_eq!(
        (
            cards[0].id.as_str(),
            cards[0].local_id.as_str(),
            cards[0].name.as_str()
        ),
        ("dc1-1", "1", "Team Magma's Numel")
    );
    assert!(cards.iter().any(|c| c.id == "dc1-6"));
    assert_eq!(
        (cards[2].id.as_str(), cards[2].local_id.as_str()),
        ("dc1-34", "34")
    );
}

#[test]
fn records_every_bucket_and_maps_only_certain_ones() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"tcgplayer":{"unit":"USD","updated":"2026-11-24T00:00:00Z",
            "1st-edition-holofoil":{"productId":1,"marketPrice":900,"lowPrice":800},
            "unlimited-holofoil":{"productId":1,"marketPrice":300},
            "mystery-foil":{"productId":2,"marketPrice":5},
            "normal":{"productId":3,"marketPrice":0}}}"#,
    )
    .unwrap();
    let p = parse_prices(Some(&v));
    let get = |b: &str| p.iter().find(|q| q.bucket == b).unwrap();
    assert_eq!(p.len(), 4);
    assert_eq!(
        get("1st-edition-holofoil").variant,
        Some(Variant::FirstEditionHolofoil)
    );
    assert_eq!(
        get("unlimited-holofoil").variant,
        Some(Variant::UnlimitedHolofoil)
    );
    assert_eq!(get("mystery-foil").variant, None);
    assert_eq!(get("normal").market, None, "a zero price is no price");
    assert_eq!(
        tcgplayer_bucket_variant("reverse-holofoil"),
        Some(Variant::ReverseHolofoil)
    );
    assert_eq!(tcgplayer_bucket_variant("holo"), None);
}

#[test]
fn unreadable_card_is_bad_data() {
    assert!(matches!(
        parse_card(Lang::En, "{\"name\":\"x\"}"),
        Err(FetchError::BadData(_))
    ));
    assert!(matches!(
        parse_card(Lang::En, "not json"),
        Err(FetchError::BadData(_))
    ));
}

#[test]
fn card_cache_hit_miss_and_expiry() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_cards_dc1-6.json"))
        .push(ok("en_cards_dc1-6.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    let a = td.card(Lang::En, "dc1-6").unwrap().unwrap();
    assert_eq!(
        t.urls(),
        vec!["https://api.tcgdex.net/v2/en/cards/dc1-6".to_string()]
    );
    assert_eq!(
        t.sent.borrow()[0].header("accept"),
        Some("application/json")
    );
    clock.advance_ms(23 * HOUR_MS);
    assert_eq!(td.card(Lang::En, "dc1-6").unwrap().unwrap(), a);
    assert_eq!(t.count(), 1, "a 23 h old card comes from the cache");
    clock.advance_ms(2 * HOUR_MS);
    td.card(Lang::En, "dc1-6").unwrap().unwrap();
    assert_eq!(t.count(), 2, "a 25 h old card is fetched again");
    assert!(db
        .cache_get(tcgdex::KIND_CARD, Lang::En, "dc1-6")
        .unwrap()
        .is_some());
}

#[test]
fn stale_card_is_used_when_the_source_is_down() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    let down = || Response {
        status: 503,
        headers: vec![],
        body: "No available server".into(),
    };
    // A 503 and its one retry.
    t.push(ok("en_cards_dc1-6.json")).push(down()).push(down());
    let mut td = Tcgdex::new(&db, &t, &clock);
    td.card(Lang::En, "dc1-6").unwrap();
    clock.advance_ms(2 * DAY_MS);
    assert_eq!(td.card(Lang::En, "dc1-6").unwrap().unwrap().id, "dc1-6");
    assert_eq!(t.count(), 3);
    // Without a cached copy, the same 503 is an error.
    t.push(down()).push(down());
    assert!(matches!(
        td.card(Lang::En, "xy1-121"),
        Err(LookupError::Unavailable(_))
    ));
}

#[test]
fn not_found_is_none_and_cached_for_seven_days() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(not_found()).push(not_found());
    let mut td = Tcgdex::new(&db, &t, &clock);
    assert_eq!(td.card(Lang::En, "dc1-999").unwrap(), None);
    assert!(db
        .cache_get(tcgdex::KIND_CARD_MISSING, Lang::En, "dc1-999")
        .unwrap()
        .is_some());
    clock.advance_ms(6 * DAY_MS);
    assert_eq!(td.card(Lang::En, "dc1-999").unwrap(), None);
    assert_eq!(td.card_fresh(Lang::En, "dc1-999").unwrap(), None);
    assert_eq!(
        t.count(),
        1,
        "a remembered 404 isn't asked again within 7 days"
    );
    clock.advance_ms(2 * DAY_MS);
    assert_eq!(td.card(Lang::En, "dc1-999").unwrap(), None);
    assert_eq!(t.count(), 2);
}

#[test]
fn set_listing_cached_for_thirty_days() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_sets_dc1.json"))
        .push(ok("en_sets_dc1.json"))
        .push(not_found());
    let mut td = Tcgdex::new(&db, &t, &clock);
    assert_eq!(td.set_cards(Lang::En, "dc1").unwrap().unwrap().len(), 3);
    assert_eq!(t.urls()[0], "https://api.tcgdex.net/v2/en/sets/dc1");
    clock.advance_ms(29 * DAY_MS);
    td.set_cards(Lang::En, "dc1").unwrap().unwrap();
    assert_eq!(t.count(), 1);
    clock.advance_ms(2 * DAY_MS);
    td.set_cards(Lang::En, "dc1").unwrap().unwrap();
    assert_eq!(t.count(), 2);
    assert_eq!(td.set_cards(Lang::Ja, "nope").unwrap(), None);
    assert!(db
        .cache_get(tcgdex::KIND_SET_MISSING, Lang::Ja, "nope")
        .unwrap()
        .is_some());
}

#[test]
fn card_fresh_bypasses_the_card_cache() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_cards_dc1-6.json"))
        .push(ok("en_cards_dc1-6.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    td.card(Lang::En, "dc1-6").unwrap();
    td.card_fresh(Lang::En, "dc1-6").unwrap().unwrap();
    assert_eq!(t.count(), 2);
}

#[test]
fn unreadable_answer_is_not_cached() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(200, "{\"oops\":true}"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    assert!(matches!(
        td.card(Lang::En, "dc1-6"),
        Err(LookupError::BadData(_))
    ));
    assert!(db
        .cache_get(tcgdex::KIND_CARD, Lang::En, "dc1-6")
        .unwrap()
        .is_none());
}

#[test]
fn ids_cannot_change_the_path() {
    assert_eq!(path_segment("sv03.5-001"), "sv03.5-001");
    assert_eq!(path_segment("a/b?c d#"), "a%2Fb%3Fc%20d%23");
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(not_found());
    let mut td = Tcgdex::new(&db, &t, &clock);
    assert_eq!(td.card(Lang::En, "..").unwrap(), None);
    assert_eq!(td.card(Lang::En, "").unwrap(), None);
    assert_eq!(t.count(), 0);
    td.card(Lang::Ja, "../sets/x").unwrap();
    assert_eq!(
        t.urls()[0],
        "https://api.tcgdex.net/v2/ja/cards/..%2Fsets%2Fx"
    );
}

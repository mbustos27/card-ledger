//! One real request to TCGdex. Ignored by default: `cargo test -p card-ledger-fetch -- --ignored`.

use card_ledger_core::card::{CardLookup, Lang};
use card_ledger_core::db::Db;
use card_ledger_fetch::{Http, SystemClock, Tcgdex};

#[test]
#[ignore = "network: fetches dc1-6 from api.tcgdex.net"]
fn live_dc1_6() {
    let db = Db::open_in_memory().unwrap();
    let (http, clock) = (Http::new(), SystemClock);
    let mut t = Tcgdex::new(&db, &http, &clock);
    let c = t
        .card(Lang::En, "dc1-6")
        .expect("TCGdex answered")
        .expect("dc1-6 exists");
    assert_eq!(c.set_id, "dc1");
    assert!(c.name.contains("Kyogre"), "{}", c.name);
    assert_eq!(c.dex_ids.first(), Some(&382));
    let tp: Vec<_> = c
        .prices
        .iter()
        .filter(|p| p.source == "tcgplayer")
        .collect();
    assert!(!tp.is_empty(), "no TCGplayer price: {:?}", c.prices);
    assert!(tp
        .iter()
        .all(|p| p.currency == "USD" && p.product_id.is_some()));
    println!("{:#?}", c.prices);
    // The second ask comes from the cache.
    t.card(Lang::En, "dc1-6").unwrap();
    assert_eq!(t.requests(), 1);
}

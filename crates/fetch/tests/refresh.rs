mod common;

use card_ledger_core::card::{CardLookup, Lang};
use card_ledger_fetch::{
    refresh_prices, FakeTransport, FetchError, Progress, RefreshReport, Response, Step, Tcgdex,
};
use common::*;

const CF_PAGE: &str = "<html><head><title>Just a moment...</title></head></html>";

fn cards(ids: &[&str]) -> Vec<(Lang, String)> {
    ids.iter().map(|i| (Lang::En, i.to_string())).collect()
}

fn run(
    db: &card_ledger_core::db::Db,
    td: &mut Tcgdex<'_>,
    ids: &[&str],
    max_age_h: u64,
) -> (RefreshReport, Vec<(usize, String, Step)>) {
    let mut seen = Vec::new();
    let r = refresh_prices(db, td, &cards(ids), max_age_h, &mut |p: &Progress<'_>| {
        seen.push((p.done, p.card_id.to_string(), p.step.clone()))
    });
    (r, seen)
}

fn unavailable() -> Response {
    Response {
        status: 503,
        headers: vec![],
        body: "No available server".into(),
    }
}

#[test]
fn fetches_stale_skips_fresh_and_keeps_history() {
    let (db, path) = file_db("history");
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_cards_dc1-6.json"))
        .push(ok("en_cards_xy1-121.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);

    let (r, seen) = run(&db, &mut td, &["dc1-6", "xy1-121", "dc1-6"], 24);
    assert_eq!(
        (r.total, r.fetched, r.skipped_fresh, r.failed, r.requests),
        (2, 2, 0, 0, 2)
    );
    assert_eq!(r.stop, None);
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1],
        (2, "xy1-121".to_string(), Step::Fetched { quotes: 4 })
    );
    let q = db.latest_quotes(Lang::En, "dc1-6").unwrap();
    assert_eq!(q.len(), 3);
    assert_eq!(q[0].fetched_at, "2026-11-24T22:53:02Z");

    // An hour later: both fresh, nothing sent.
    clock.advance_ms(HOUR_MS);
    let (r, _) = run(&db, &mut td, &["dc1-6", "xy1-121"], 24);
    assert_eq!((r.fetched, r.skipped_fresh, r.requests), (0, 2, 0));

    // A day later: fetched again, and the first rows are kept.
    clock.advance_ms(DAY_MS);
    t.push(ok("en_cards_dc1-6.json"));
    let (r, _) = run(&db, &mut td, &["dc1-6"], 24);
    assert_eq!((r.fetched, r.requests), (1, 1));
    assert_eq!(
        count_rows(
            &path,
            "SELECT COUNT(*) FROM price_quotes WHERE card_id='dc1-6' AND bucket='holofoil'"
        ),
        2
    );
    assert_eq!(
        count_rows(
            &path,
            "SELECT COUNT(DISTINCT fetched_at) FROM price_quotes WHERE card_id='dc1-6'"
        ),
        2
    );
    assert_eq!(
        db.last_price_fetch(Lang::En, "dc1-6").unwrap().as_deref(),
        Some("2026-11-25T23:53:02Z")
    );
}

#[test]
fn refresh_ignores_the_24h_card_cache_when_prices_are_older_than_max_age() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_cards_dc1-6.json"))
        .push(ok("en_cards_dc1-6.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    run(&db, &mut td, &["dc1-6"], 1);
    clock.advance_ms(2 * HOUR_MS);
    // The card JSON is 2 h old (fresh for the 24 h card cache) but prices are over 1 h old.
    let (r, _) = run(&db, &mut td, &["dc1-6"], 1);
    assert_eq!((r.fetched, r.requests), (1, 1));
    assert_eq!(t.count(), 2);
}

#[test]
fn prices_from_a_recent_identity_fetch_are_saved_without_asking_again() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_cards_xy1-121.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    td.card(Lang::En, "xy1-121").unwrap();
    clock.advance_ms(HOUR_MS);
    let (r, seen) = run(&db, &mut td, &["xy1-121"], 24);
    assert_eq!((r.from_cache, r.fetched, r.requests), (1, 0, 0));
    assert_eq!(seen[0].2, Step::FromCache { quotes: 4 });
    let q = db.latest_quotes(Lang::En, "xy1-121").unwrap();
    assert_eq!(
        q[0].fetched_at, "2026-11-24T22:53:02Z",
        "dated when the JSON was fetched"
    );
}

#[test]
fn a_card_with_no_price_is_not_fetched_again_until_it_is_old() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(200, r#"{"id":"x1-1","name":"Nobody","localId":"1","set":{"id":"x1","name":"X"},"pricing":{"cardmarket":null,"tcgplayer":null}}"#));
    let mut td = Tcgdex::new(&db, &t, &clock);
    let (r, _) = run(&db, &mut td, &["x1-1"], 24);
    assert_eq!((r.fetched, r.no_price), (1, 1));
    clock.advance_ms(HOUR_MS);
    let (r, _) = run(&db, &mut td, &["x1-1"], 24);
    assert_eq!((r.skipped_fresh, r.requests), (1, 0));
}

#[test]
fn missing_cards_are_counted_and_remembered() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(not_found()).push(ok("en_cards_dc1-6.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    let (r, _) = run(&db, &mut td, &["dc1-999", "dc1-6"], 24);
    assert_eq!((r.missing, r.fetched), (1, 1));
    clock.advance_ms(2 * DAY_MS);
    t.push(ok("en_cards_dc1-6.json"));
    let (r, _) = run(&db, &mut td, &["dc1-999", "dc1-6"], 24);
    assert_eq!((r.missing, r.fetched, r.requests), (1, 1, 1));
}

#[test]
fn rate_limit_stops_cleanly_and_keeps_what_was_saved() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_cards_dc1-6.json"));
    t.push(Response::json(429, "{}").with_header("Retry-After", "2"));
    t.push(Response::json(429, "{}"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    let (r, seen) = run(&db, &mut td, &["dc1-6", "xy1-121", "g1-RC7"], 24);
    assert_eq!((r.fetched, r.failed, r.requests), (1, 1, 3));
    assert!(matches!(r.stop, Some(FetchError::RateLimited(_))));
    assert_eq!(seen.len(), 2, "g1-RC7 never started");
    assert_eq!(t.count(), 3);
    assert!(clock.sleeps.borrow().contains(&2_000));
    assert_eq!(db.latest_quotes(Lang::En, "dc1-6").unwrap().len(), 3);
    assert!(db.latest_quotes(Lang::En, "xy1-121").unwrap().is_empty());
}

#[test]
fn challenge_stops_cleanly() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(ok("en_cards_dc1-6.json"));
    t.push(Response {
        status: 403,
        headers: vec![("content-type".into(), "text/html".into())],
        body: CF_PAGE.into(),
    });
    let mut td = Tcgdex::new(&db, &t, &clock);
    let (r, _) = run(&db, &mut td, &["dc1-6", "xy1-121", "g1-RC7"], 24);
    assert_eq!((r.fetched, r.requests), (1, 2));
    assert!(matches!(r.stop, Some(FetchError::Challenge(_))));
    assert_eq!(db.latest_quotes(Lang::En, "dc1-6").unwrap().len(), 3);
}

#[test]
fn one_unavailable_answer_is_a_failure_three_in_a_row_stop() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    // Each failure is a 503 plus its one retry.
    t.push(unavailable())
        .push(unavailable())
        .push(ok("en_cards_xy1-121.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    let (r, _) = run(&db, &mut td, &["dc1-6", "xy1-121"], 24);
    assert_eq!((r.failed, r.fetched, r.stop.is_none()), (1, 1, true));
    assert_eq!(r.failures.len(), 1);
    assert!(r.failures[0].starts_with("en dc1-6:"), "{:?}", r.failures);

    for _ in 0..6 {
        t.push(unavailable());
    }
    let (r, _) = run(&db, &mut td, &["a-1", "a-2", "a-3", "a-4"], 24);
    assert_eq!((r.failed, r.requests), (3, 6));
    assert!(matches!(r.stop, Some(FetchError::Unavailable(_))));
}

#[test]
fn fresh_skips_between_outages_do_not_reset_the_count() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    // f-1 and f-2 have prices from a minute ago, so they are skipped without a request.
    let c =
        card_ledger_fetch::tcgdex::parse_card(Lang::En, &fixture("en_cards_dc1-6.json")).unwrap();
    for id in ["f-1", "f-2"] {
        db.save_quotes(Lang::En, id, &c.prices, "2026-11-24T22:52:02Z")
            .unwrap();
    }
    for _ in 0..8 {
        t.push(unavailable());
    }
    let mut td = Tcgdex::new(&db, &t, &clock);
    let (r, _) = run(
        &db,
        &mut td,
        &["a-1", "f-1", "a-2", "f-2", "a-3", "a-4"],
        24,
    );
    assert!(
        matches!(r.stop, Some(FetchError::Unavailable(_))),
        "{:?}",
        r.stop
    );
    assert_eq!(
        (r.failed, r.skipped_fresh, r.requests),
        (3, 2, 6),
        "stops after the third network failure"
    );
}

#[test]
fn a_json_403_stops_the_run_at_once() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(403, r#"{"error":"forbidden"}"#))
        .push(ok("en_cards_dc1-6.json"));
    let mut td = Tcgdex::new(&db, &t, &clock);
    let (r, seen) = run(&db, &mut td, &["a-1", "dc1-6"], 24);
    assert!(
        matches!(r.stop, Some(FetchError::Refused { status: 403, .. })),
        "{:?}",
        r.stop
    );
    assert_eq!((seen.len(), r.requests), (1, 1));
}

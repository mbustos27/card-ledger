mod common;

use card_ledger_core::card::{Lang, MarketPrice};
use card_ledger_core::db::Db;
use card_ledger_core::model::Variant;
use card_ledger_fetch::justtcg::keystore::{self, KeyStore, MemoryStore};
use card_ledger_fetch::justtcg::{
    self, find_key, key_status, parse_batch, quota, take_request, JustTcg, KeySource, SETTING_DAY,
    SETTING_MONTH,
};
use card_ledger_fetch::tcgdex::parse_card;
use card_ledger_fetch::transport::Method;
use card_ledger_fetch::{FakeTransport, FetchError, Response};
use common::*;

const KEY: &str = "tcg_test_key_not_real";

fn now_s() -> u64 {
    T0_MS / 1000
}

/// Saves TCGdex quotes for dc1-6, xy1-121 and g1-RC7 (which has no TCGplayer product id).
fn seed(db: &Db) {
    for (f, id) in [
        ("en_cards_dc1-6.json", "dc1-6"),
        ("en_cards_xy1-121.json", "xy1-121"),
        ("en_cards_g1-RC7.json", "g1-RC7"),
    ] {
        let c = parse_card(Lang::En, &fixture(f)).unwrap();
        db.save_quotes(Lang::En, id, &c.prices, "2026-11-24T22:00:00Z")
            .unwrap();
    }
}

fn cards(ids: &[&str]) -> Vec<(Lang, String)> {
    ids.iter().map(|i| (Lang::En, i.to_string())).collect()
}

#[test]
fn no_key_means_off_and_nothing_sent() {
    let clock = clock();
    let t = FakeTransport::new();
    assert!(JustTcg::with_key("", &t, &clock).is_none());
    assert!(JustTcg::with_key("   ", &t, &clock).is_none());
    let empty = MemoryStore::new();
    assert!(JustTcg::from_parts(None, &empty, &t, &clock)
        .unwrap()
        .is_none());
    assert!(JustTcg::from_parts(Some("  "), &empty, &t, &clock)
        .unwrap()
        .is_none());
    assert!(
        JustTcg::from_parts(None, &MemoryStore::holding("   "), &t, &clock)
            .unwrap()
            .is_none()
    );
    if std::env::var(justtcg::ENV_KEY).is_err() {
        assert!(JustTcg::from_config(&empty, &t, &clock).unwrap().is_none());
    }
    assert_eq!(t.count(), 0);
}

const STORED: &str = "test-key-0000abcd";
const ENV: &str = "test-env-key-9999wxyz";

/// Runs one batch with a JustTCG built from `env` and `store`; returns the x-api-key sent.
fn key_sent(env: Option<&str>, store: &MemoryStore) -> Option<String> {
    let db = mem_db();
    seed(&db);
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(
        200,
        &fixture("justtcg_batch_from_docs.json"),
    ));
    let mut j = JustTcg::from_parts(env, store, &t, &clock).unwrap()?;
    j.refresh_conditions(&db, &cards(&["dc1-6"]), 24, &mut |_| {});
    let sent = t.sent.borrow();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].url.starts_with("https://api.justtcg.com/"),
        "{}",
        sent[0].url
    );
    sent[0].header("x-api-key").map(str::to_string)
}

#[test]
fn env_key_wins_over_the_stored_key() {
    let store = MemoryStore::holding(STORED);
    assert_eq!(key_sent(Some(ENV), &store).as_deref(), Some(ENV));
    assert_eq!(key_sent(None, &store).as_deref(), Some(STORED));
    assert_eq!(
        key_sent(Some(" "), &store).as_deref(),
        Some(STORED),
        "a blank env var counts as unset"
    );
    assert_eq!(key_sent(None, &MemoryStore::new()), None);

    let (_, src) = find_key(Some(ENV), &store).unwrap().unwrap();
    assert_eq!(src, KeySource::Env);
    let (_, src) = find_key(None, &store).unwrap().unwrap();
    assert_eq!(src, KeySource::Stored("memory (test)".into()));
}

#[test]
fn status_is_masked_to_the_last_four() {
    let s = key_status(None, &MemoryStore::new()).unwrap();
    assert_eq!(
        (s.is_set(), s.summary().as_str(), s.ends),
        (false, "not set", None)
    );

    let store = MemoryStore::holding(STORED);
    let s = key_status(None, &store).unwrap();
    assert_eq!(s.summary(), "set (ends …abcd)");
    assert_eq!(s.source, Some(KeySource::Stored("memory (test)".into())));
    assert!(!s.stored_too);
    assert!(!format!("{s:?}").contains(STORED));

    let s = key_status(Some(ENV), &store).unwrap();
    assert_eq!(
        (s.summary().as_str(), s.source.clone(), s.stored_too),
        ("set (ends …wxyz)", Some(KeySource::Env), true)
    );
    assert!(!format!("{s:?}").contains(ENV));

    // Too short to show any of it.
    assert_eq!(
        key_status(Some("abc123"), &MemoryStore::new())
            .unwrap()
            .summary(),
        "set"
    );
    assert_eq!(keystore::mask("test-key-0000abcd").as_deref(), Some("abcd"));
    assert_eq!(keystore::mask("short-key"), None);
}

#[test]
fn debug_never_shows_the_key() {
    let clock = clock();
    let t = FakeTransport::new();
    let store = MemoryStore::holding(STORED);
    let j = JustTcg::from_parts(None, &store, &t, &clock)
        .unwrap()
        .unwrap();
    let (k, _) = find_key(None, &store).unwrap().unwrap();
    for text in [format!("{j:?}"), format!("{k:?}"), format!("{store:?}")] {
        assert!(!text.contains(STORED) && !text.contains("abcd"), "{text}");
    }
}

#[test]
fn clear_turns_condition_prices_off() {
    let clock = clock();
    let t = FakeTransport::new();
    let store = MemoryStore::holding(STORED);
    assert!(store.clear().unwrap());
    assert!(!store.clear().unwrap(), "nothing left to clear");
    assert!(JustTcg::from_parts(None, &store, &t, &clock)
        .unwrap()
        .is_none());
    assert!(!key_status(None, &store).unwrap().is_set());
    assert_eq!(t.count(), 0);
}

#[test]
fn a_store_error_does_not_send_and_names_no_key() {
    struct Broken;
    impl KeyStore for Broken {
        fn location(&self) -> String {
            "broken store".into()
        }
        fn get(&self) -> Result<Option<String>, String> {
            Err("broken store: can't read".into())
        }
        fn set(&self, _: &str) -> Result<(), String> {
            Err("no".into())
        }
        fn clear(&self) -> Result<bool, String> {
            Err("no".into())
        }
    }
    let clock = clock();
    let t = FakeTransport::new();
    assert!(JustTcg::from_parts(None, &Broken, &t, &clock).is_err());
    // The env var still works without touching the store.
    assert!(JustTcg::from_parts(Some(ENV), &Broken, &t, &clock)
        .unwrap()
        .is_some());
    assert_eq!(t.count(), 0);
}

#[test]
fn file_store_round_trip() {
    let dir = std::env::temp_dir().join(format!("card-ledger-key-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let f = keystore::FileStore::new(&dir);
    assert_eq!(f.get().unwrap(), None);
    f.set(STORED).unwrap();
    assert_eq!(f.get().unwrap().as_deref(), Some(STORED));
    assert!(
        f.location().ends_with(keystore::FILE_NAME),
        "{}",
        f.location()
    );
    assert!(f.clear().unwrap());
    assert_eq!(f.get().unwrap(), None);
    assert!(!f.clear().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn key_format_check() {
    assert!(keystore::check_format(STORED).is_ok());
    for bad in [
        "",
        "too-short",
        "has space inside 12345",
        "ключ-ключ-ключ-ключ",
    ] {
        let e = keystore::check_format(bad).unwrap_err();
        assert!(bad.is_empty() || !e.contains(bad), "{e}");
    }
    assert!(keystore::check_format(&"k".repeat(keystore::MAX_LEN + 1)).is_err());
}

#[test]
fn parses_the_docs_shaped_batch() {
    let m = parse_batch(&fixture("justtcg_batch_from_docs.json")).unwrap();
    let g = &m[&100006];
    assert_eq!(g.priced.len(), 1, "the Japanese variant is skipped");
    let h = &g.priced[0];
    assert_eq!(
        (h.source.as_str(), h.variant),
        ("justtcg", Variant::Holofoil)
    );
    assert_eq!(
        (h.nm, h.lp, h.mp, h.hp, h.dmg),
        (Some(100.0), Some(80.0), Some(60.0), None, Some(30.0))
    );
    assert_eq!(h.updated.as_deref(), Some("2026-11-24T22:40:00Z"));
    let m2 = &m[&87658];
    let normal = m2
        .priced
        .iter()
        .find(|p| p.variant == Variant::Normal)
        .unwrap();
    let rev = m2
        .priced
        .iter()
        .find(|p| p.variant == Variant::ReverseHolofoil)
        .unwrap();
    assert_eq!(
        (normal.nm, normal.lp),
        (Some(5.0), Some(4.0)),
        "Sealed is not a condition"
    );
    assert_eq!((rev.nm, rev.lp), (Some(6.0), None));
    assert_eq!(m2.unknown, vec!["Cosmos Holofoil".to_string()]);
}

#[test]
fn refresh_conditions_end_to_end() {
    let db = mem_db();
    seed(&db);
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(
        200,
        &fixture("justtcg_batch_from_docs.json"),
    ));
    let mut j = JustTcg::with_key(KEY, &t, &clock).unwrap();
    let mut batches = Vec::new();
    let r = j.refresh_conditions(
        &db,
        &cards(&["dc1-6", "xy1-121", "g1-RC7", "dc1-6"]),
        24,
        &mut |b| batches.push(b),
    );
    assert_eq!(r.stop, None);
    assert_eq!(
        (
            r.cards,
            r.no_product_id,
            r.not_found,
            r.cards_priced,
            r.saved,
            r.requests
        ),
        (3, 1, 0, 2, 3, 1)
    );
    assert_eq!(r.unknown_printings, vec!["Cosmos Holofoil".to_string()]);
    assert_eq!(batches.len(), 1);

    let sent = t.sent.borrow();
    assert_eq!(sent[0].method, Method::Post);
    assert_eq!(sent[0].url, "https://api.justtcg.com/v1/cards");
    assert_eq!(sent[0].header("x-api-key"), Some(KEY));
    let body: serde_json::Value = serde_json::from_str(sent[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!([{ "tcgplayerId": "87658" }, { "tcgplayerId": "100006" }])
    );
    drop(sent);

    let g = db
        .latest_condition_prices(Lang::En, "dc1-6", Variant::Holofoil)
        .unwrap()
        .unwrap();
    assert_eq!(
        (g.prices.nm, g.fetched_at.as_str()),
        (Some(100.0), "2026-11-24T22:53:02Z")
    );
    assert!(db
        .latest_condition_prices(Lang::En, "xy1-121", Variant::ReverseHolofoil)
        .unwrap()
        .is_some());
    assert!(db
        .latest_condition_prices(Lang::En, "xy1-121", Variant::Normal)
        .unwrap()
        .is_some());
    assert_eq!(quota(&db, now_s()).unwrap().day_used, 1);
}

#[test]
fn batches_of_twenty_six_seconds_apart() {
    let db = mem_db();
    let mut ids = Vec::new();
    for i in 0..45u64 {
        let id = format!("t-{i}");
        let q = MarketPrice {
            source: "tcgplayer".into(),
            bucket: "normal".into(),
            variant: Some(Variant::Normal),
            market: Some(1.0),
            low: None,
            currency: "USD".into(),
            updated: None,
            product_id: Some(1000 + i),
        };
        db.save_quotes(Lang::En, &id, &[q], "2026-11-24T22:00:00Z")
            .unwrap();
        ids.push(id);
    }
    let clock = clock();
    let t = FakeTransport::new();
    for _ in 0..3 {
        t.push(Response::json(200, "{\"data\":[]}"));
    }
    let mut j = JustTcg::with_key(KEY, &t, &clock).unwrap();
    let list: Vec<(Lang, String)> = ids.iter().map(|i| (Lang::En, i.clone())).collect();
    let r = j.refresh_conditions(&db, &list, 24, &mut |_| {});
    assert_eq!((r.requests, r.not_found, r.saved), (3, 45, 0));
    let sizes: Vec<usize> = t
        .sent
        .borrow()
        .iter()
        .map(|s| {
            serde_json::from_str::<serde_json::Value>(s.body.as_deref().unwrap())
                .unwrap()
                .as_array()
                .unwrap()
                .len()
        })
        .collect();
    assert_eq!(sizes, vec![20, 20, 5]);
    assert_eq!(*clock.sleeps.borrow(), vec![6_000, 6_000]);
    let q = quota(&db, now_s()).unwrap();
    assert_eq!((q.day_used, q.month_used), (3, 3));
}

#[test]
fn counts_requests_per_day_and_month() {
    let db = mem_db();
    let now = now_s();
    for _ in 0..3 {
        take_request(&db, now).unwrap();
    }
    assert_eq!(
        db.setting(SETTING_DAY).unwrap().as_deref(),
        Some("2026-11-24 3")
    );
    assert_eq!(
        db.setting(SETTING_MONTH).unwrap().as_deref(),
        Some("2026-11-24:3")
    );
    // The next UTC day: the day count starts again, the month keeps counting.
    take_request(&db, now + 2 * 3_600).unwrap();
    let q = quota(&db, now + 2 * 3_600).unwrap();
    assert_eq!((q.day_used, q.month_used), (1, 4));
    // Over 31 days later, the old days have left the month window.
    let q = quota(&db, now + 40 * 86_400).unwrap();
    assert_eq!((q.day_used, q.month_used), (0, 0));
}

#[test]
fn refuses_before_passing_the_daily_limit() {
    let db = mem_db();
    seed(&db);
    db.set_setting(SETTING_DAY, "2026-11-24 100").unwrap();
    let clock = clock();
    let t = FakeTransport::new();
    let mut j = JustTcg::with_key(KEY, &t, &clock).unwrap();
    let r = j.refresh_conditions(&db, &cards(&["dc1-6"]), 24, &mut |_| {});
    assert!(
        matches!(&r.stop, Some(FetchError::OverBudget(m)) if m.contains("today")),
        "{:?}",
        r.stop
    );
    assert_eq!((t.count(), r.requests), (0, 0));
}

#[test]
fn refuses_before_passing_the_monthly_limit() {
    let db = mem_db();
    seed(&db);
    db.set_setting(
        SETTING_MONTH,
        "2026-11-01:400,2026-11-10:350,2026-11-20:250,2026-10-01:900",
    )
    .unwrap();
    let clock = clock();
    let t = FakeTransport::new();
    let mut j = JustTcg::with_key(KEY, &t, &clock).unwrap();
    let r = j.refresh_conditions(&db, &cards(&["dc1-6"]), 24, &mut |_| {});
    assert!(
        matches!(&r.stop, Some(FetchError::OverBudget(m)) if m.contains("1000 of 1000")),
        "{:?}",
        r.stop
    );
    assert_eq!(t.count(), 0);
}

#[test]
fn quota_429_stops_and_marks_the_day_used_up() {
    let db = mem_db();
    seed(&db);
    let clock = clock();
    let t = FakeTransport::new();
    t.push(
        Response::json(
            429,
            r#"{"error":"Daily limit exceeded","code":"DAILY_LIMIT_EXCEEDED"}"#,
        )
        .with_header("Retry-After", "5"),
    );
    let mut j = JustTcg::with_key(KEY, &t, &clock).unwrap();
    let r = j.refresh_conditions(&db, &cards(&["dc1-6"]), 24, &mut |_| {});
    assert!(
        matches!(&r.stop, Some(FetchError::RateLimited(m)) if m.contains("DAILY_LIMIT_EXCEEDED")),
        "{:?}",
        r.stop
    );
    assert_eq!(t.count(), 1, "a used-up quota is not retried");
    assert!(clock.sleeps.borrow().is_empty());
    assert_eq!(quota(&db, now_s()).unwrap().day_used, 100);
}

#[test]
fn a_bad_key_stops_and_the_key_is_not_in_the_report() {
    let db = mem_db();
    seed(&db);
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(
        401,
        r#"{"error":"Invalid API key","code":"INVALID_API_KEY"}"#,
    ));
    let mut j = JustTcg::with_key(KEY, &t, &clock).unwrap();
    let r = j.refresh_conditions(&db, &cards(&["dc1-6", "xy1-121"]), 24, &mut |_| {});
    assert!(
        matches!(&r.stop, Some(FetchError::Refused { status: 401, why }) if why.contains("INVALID_API_KEY")),
        "{:?}",
        r.stop
    );
    assert!(!format!("{r:?} {}", r.stop.as_ref().unwrap()).contains(KEY));
}

#[test]
fn two_refreshes_within_the_hour_send_once() {
    let db = mem_db();
    seed(&db);
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(
        200,
        &fixture("justtcg_batch_from_docs.json"),
    ));
    t.push(Response::json(
        200,
        &fixture("justtcg_batch_from_docs.json"),
    ));
    let mut j = JustTcg::with_key(KEY, &t, &clock).unwrap();
    let list = cards(&["dc1-6", "xy1-121", "g1-RC7"]);
    let r = j.refresh_conditions(&db, &list, 24, &mut |_| {});
    assert_eq!((r.requests, r.skipped_fresh, r.cards_priced), (1, 0, 2));

    clock.advance_ms(HOUR_MS / 2);
    let r = j.refresh_conditions(&db, &list, 24, &mut |_| {});
    assert_eq!((r.requests, r.skipped_fresh, r.saved), (0, 2, 0), "{r:?}");
    assert_eq!(t.count(), 1);

    // Once the rows are older than the limit, they are asked for again.
    clock.advance_ms(DAY_MS);
    let r = j.refresh_conditions(&db, &list, 24, &mut |_| {});
    assert_eq!((r.requests, r.skipped_fresh), (1, 0));
    assert_eq!(t.count(), 2);
}

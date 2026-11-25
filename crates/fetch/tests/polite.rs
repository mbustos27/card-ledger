mod common;

use card_ledger_core::card::{CardLookup, Lang, LookupError};
use card_ledger_fetch::polite::{detect_challenge, retry_after_secs, MIN_GAP_MS};
use card_ledger_fetch::{
    Clock, FakeTransport, FetchError, NetError, Polite, Request, Response, Tcgdex, USER_AGENT,
};
use common::*;

/// A minimal bot-check sample: only the marker the detector looks for, no copied page text.
const CF_PAGE: &str = "<html><head><title>Just a moment...</title></head></html>";

fn j(body: &str) -> Response {
    Response::json(200, body)
}

#[test]
fn honest_user_agent() {
    assert_eq!(USER_AGENT, "card-ledger/0.1 (personal collection tool)");
}

#[test]
fn keeps_the_gap_between_requests_to_one_host() {
    let clock = clock();
    let t = FakeTransport::new();
    for _ in 0..4 {
        t.push(j("{}"));
    }
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    p.get_json("https://api.tcgdex.net/v2/en/cards/a").unwrap();
    p.get_json("https://api.tcgdex.net/v2/en/cards/b").unwrap();
    assert_eq!(*clock.sleeps.borrow(), vec![500]);
    p.get_json("https://other.example/x").unwrap();
    assert_eq!(
        *clock.sleeps.borrow(),
        vec![500],
        "another host has its own gap"
    );
    clock.advance_ms(2_000);
    p.get_json("https://api.tcgdex.net/v2/en/cards/c").unwrap();
    assert_eq!(
        *clock.sleeps.borrow(),
        vec![500],
        "no wait once the gap has passed"
    );
    assert_eq!(p.requests, 4);
}

#[test]
fn gap_is_never_below_500_ms() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(j("{}")).push(j("{}"));
    let mut p = Polite::new(&t, &clock, 10);
    p.get_json("https://h/a").unwrap();
    p.get_json("https://h/b").unwrap();
    assert_eq!(*clock.sleeps.borrow(), vec![500]);
}

#[test]
fn a_429_is_waited_out_once() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(429, "{}").with_header("Retry-After", "7"))
        .push(j("{\"ok\":1}"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    let r = p.get_json("https://h/a").unwrap();
    assert_eq!(r.body, "{\"ok\":1}");
    assert_eq!(*clock.sleeps.borrow(), vec![7_000]);
    assert_eq!(t.count(), 2);
}

#[test]
fn a_second_429_stops_the_run_for_good() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(429, "{}").with_header("Retry-After", "1"))
        .push(Response::json(429, "{}"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    assert!(matches!(
        p.get_json("https://h/a"),
        Err(FetchError::RateLimited(_))
    ));
    assert_eq!(t.count(), 2);
    t.push(j("{}"));
    assert!(matches!(
        p.get_json("https://h/b"),
        Err(FetchError::RateLimited(_))
    ));
    assert_eq!(t.count(), 2, "nothing more is sent after a stop");
}

#[test]
fn a_429_on_a_later_request_after_one_wait_stops() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(429, "{}").with_header("Retry-After", "1"))
        .push(j("{}"))
        .push(Response::json(429, "{}").with_header("Retry-After", "1"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    p.get_json("https://h/a").unwrap();
    assert!(matches!(
        p.get_json("https://h/b"),
        Err(FetchError::RateLimited(_))
    ));
    assert_eq!(t.count(), 3);
}

#[test]
fn a_long_retry_after_stops_without_waiting() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(429, "{}").with_header("Retry-After", "600"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    let e = p.get_json("https://h/a").unwrap_err();
    assert!(
        matches!(&e, FetchError::RateLimited(m) if m.contains("600")),
        "{e:?}"
    );
    assert!(clock.sleeps.borrow().is_empty());
    assert_eq!(t.count(), 1);
}

#[test]
fn retry_after_as_seconds_or_http_date() {
    let now = T0_MS / 1000; // 2026-11-24T22:53:02Z
    assert_eq!(retry_after_secs(Some(" 30 "), now), Some(30));
    assert_eq!(
        retry_after_secs(Some("Tue, 24 Nov 2026 22:54:02 GMT"), now),
        Some(60)
    );
    assert_eq!(
        retry_after_secs(Some("Tue, 24 Nov 2026 22:00:00 GMT"), now),
        Some(0)
    );
    assert_eq!(retry_after_secs(Some("soon"), now), None);
    assert_eq!(retry_after_secs(None, now), None);
}

#[test]
fn a_challenge_page_stops_the_run() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response {
        status: 403,
        headers: vec![("content-type".into(), "text/html".into())],
        body: CF_PAGE.into(),
    });
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    let e = p.get_json("https://h/a").unwrap_err();
    assert!(
        matches!(&e, FetchError::Challenge(m) if m.contains("Just a moment")),
        "{e:?}"
    );
    t.push(j("{}"));
    assert!(matches!(
        p.get_json("https://h/b"),
        Err(FetchError::Challenge(_))
    ));
    assert_eq!(
        t.count(),
        1,
        "nothing is sent after a bot check, and nothing is retried"
    );
}

#[test]
fn html_where_json_was_expected_is_a_challenge() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response {
        status: 200,
        headers: vec![],
        body: "<html><body>Please verify you are human</body></html>".into(),
    });
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    assert!(
        matches!(p.get_json("https://h/a"), Err(FetchError::Challenge(m)) if m.contains("HTML"))
    );
}

#[test]
fn challenge_markers() {
    let h = |k: &str, v: &str| Response::json(200, "{}").with_header(k, v);
    assert!(detect_challenge(&h("cf-mitigated", "challenge")).is_some());
    assert!(detect_challenge(&Response::json(503, CF_PAGE)).is_some());
    assert!(detect_challenge(&Response::json(200, &fixture("en_cards_dc1-6.json"))).is_none());
    assert!(
        detect_challenge(&Response::json(404, &fixture("en_cards_dc1-999_404.json"))).is_none()
    );
}

#[test]
fn server_errors_and_timeouts_are_unavailable_and_not_sticky() {
    let clock = clock();
    let t = FakeTransport::new();
    // A 503 is retried once after a pause; a second 503 is reported.
    t.push(Response {
        status: 503,
        headers: vec![],
        body: "No available server".into(),
    });
    t.push(Response {
        status: 503,
        headers: vec![],
        body: "No available server".into(),
    });
    t.push_err(NetError::Timeout("global".into()));
    t.push(j("{}"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    assert!(
        matches!(p.get_json("https://h/a"), Err(FetchError::Unavailable(m)) if m.contains("503"))
    );
    assert_eq!(t.count(), 2, "one retry, no more");
    assert!(
        matches!(p.get_json("https://h/a"), Err(FetchError::Unavailable(m)) if m.contains("timed out"))
    );
    assert!(p.get_json("https://h/a").is_ok());
    assert_eq!(t.count(), 4);
}

#[test]
fn a_passing_503_is_retried_once_after_a_pause() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response {
        status: 503,
        headers: vec![],
        body: "No available server".into(),
    });
    t.push(j("{}"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    let before = clock.now_ms();
    assert!(p.get_json("https://h/a").is_ok());
    assert_eq!(t.count(), 2);
    assert!(clock.now_ms() - before >= card_ledger_fetch::polite::TRANSIENT_RETRY_MS);
}

#[test]
fn challenge_reaches_card_lookup_callers_as_a_stop() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response {
        status: 503,
        headers: vec![],
        body: CF_PAGE.into(),
    });
    let mut td = Tcgdex::new(&db, &t, &clock);
    let lookup: &mut dyn CardLookup = &mut td;
    assert!(
        matches!(lookup.card(Lang::En, "dc1-6"), Err(LookupError::RateLimited(m)) if m.contains("bot check"))
    );
    assert!(lookup.card(Lang::En, "xy1-121").is_err());
    assert_eq!(t.count(), 1);
}

#[test]
fn request_debug_hides_header_values() {
    let r = Request::post_json("https://api.justtcg.com/v1/cards", "[]".into())
        .with_header("x-api-key", "tcg_secret_value_123");
    let shown = format!("{r:?}");
    assert!(!shown.contains("tcg_secret_value_123"), "{shown}");
    assert!(shown.contains("x-api-key"));
}

#[test]
fn a_429_then_a_503_is_not_sent_a_third_time() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(429, "{}").with_header("Retry-After", "1"));
    t.push(Response {
        status: 503,
        headers: vec![],
        body: "No available server".into(),
    });
    t.push(j("{}"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    assert!(
        matches!(p.get_json("https://h/a"), Err(FetchError::Unavailable(m)) if m.contains("503"))
    );
    assert_eq!(t.count(), 2, "one retry per request, the 429's wait was it");
}

#[test]
fn a_503_with_a_long_retry_after_is_not_retried() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response {
        status: 503,
        headers: vec![("Retry-After".into(), "120".into())],
        body: "maintenance".into(),
    });
    t.push(j("{}"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    let e = p.get_json("https://h/a").unwrap_err();
    assert!(
        matches!(&e, FetchError::Unavailable(m) if m.contains("120")),
        "{e:?}"
    );
    assert_eq!(t.count(), 1);
    assert!(
        clock.sleeps.borrow().is_empty(),
        "no wait for a retry that isn't sent"
    );
    // Not sticky: the next request goes out.
    assert!(p.get_json("https://h/b").is_ok());
}

#[test]
fn a_503_with_a_short_retry_after_waits_at_least_the_usual_pause() {
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response {
        status: 503,
        headers: vec![("Retry-After".into(), "1".into())],
        body: "busy".into(),
    });
    t.push(j("{}"));
    let mut p = Polite::new(&t, &clock, MIN_GAP_MS);
    assert!(p.get_json("https://h/a").is_ok());
    assert_eq!(t.count(), 2);
    assert_eq!(
        *clock.sleeps.borrow(),
        vec![card_ledger_fetch::polite::TRANSIENT_RETRY_MS]
    );
}

#[test]
fn a_json_403_reaches_card_lookup_callers_as_a_stop() {
    let db = mem_db();
    let clock = clock();
    let t = FakeTransport::new();
    t.push(Response::json(403, r#"{"error":"forbidden"}"#));
    let mut td = Tcgdex::new(&db, &t, &clock);
    let lookup: &mut dyn CardLookup = &mut td;
    let e = lookup.card(Lang::En, "dc1-6").unwrap_err();
    assert!(
        matches!(&e, LookupError::RateLimited(m) if m.contains("403")),
        "{e:?}"
    );
}

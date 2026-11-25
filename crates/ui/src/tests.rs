use super::*;
use card_ledger_core::import;
use std::sync::atomic::{AtomicU32, Ordering};

const HOST: Option<&str> = Some("127.0.0.1:8785");
const JSON: Option<&str> = Some("application/json");
const OURS: Option<&str> = Some("http://127.0.0.1:8785");
const SAMPLE: &str = include_str!("../../core/tests/fixtures/sample.csv");

/// A temporary data folder per test, never the real one. Removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "card-ledger-ui-tests-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An app over a real database file holding the sample export.
fn app_with(dir: &TempDir, refresh: Option<RefreshHook>) -> App {
    let mut db = Db::open(&dir.0.join("ledger.db")).unwrap();
    db.import(&import::parse(SAMPLE.as_bytes()).unwrap())
        .unwrap();
    App::with(db, &dir.0, 8785, refresh)
}

fn get(a: &App, url: &str) -> Reply {
    a.route("GET", url, HOST, None, None, None, "")
}

fn post(a: &App, path: &str, body: &str) -> Reply {
    a.route("POST", path, HOST, Some("1"), JSON, OURS, body)
}

fn body(r: &Reply) -> Value {
    serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("{e}: {}", r.body))
}

fn first_copy(a: &App) -> i64 {
    body(&get(a, "/api/ledger"))["cards"][0]["copy_id"]
        .as_i64()
        .unwrap()
}

fn card(a: &App, id: i64) -> Value {
    body(&get(a, "/api/ledger"))["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["copy_id"] == id)
        .cloned()
        .unwrap()
}

#[test]
fn other_hosts_are_refused() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    // A DNS-rebinding page arrives with its own host name.
    assert_eq!(
        a.route(
            "GET",
            "/api/ledger",
            Some("evil.example:8785"),
            None,
            None,
            None,
            ""
        )
        .status,
        421
    );
    assert_eq!(a.route("GET", "/", None, None, None, None, "").status, 421);
    assert_eq!(
        a.route("GET", "/", Some("127.0.0.1:9999"), None, None, None, "")
            .status,
        421
    );
    assert_eq!(
        a.route("GET", "/", Some("localhost:8785"), None, None, None, "")
            .status,
        200
    );
    assert_eq!(get(&a, "/").status, 200);
}

#[test]
fn api_reads_refuse_other_origins() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    let read = |o: Option<&str>| {
        a.route("GET", "/api/ledger", HOST, None, None, o, "")
            .status
    };
    assert_eq!(read(Some("https://evil.example")), 403);
    assert_eq!(read(Some("null")), 403);
    assert_eq!(read(Some("http://127.0.0.1:9999")), 403);
    assert_eq!(read(OURS), 200);
    assert_eq!(read(None), 200);
    let id = first_copy(&a);
    let w = a.route(
        "POST",
        &format!("/api/copy/{id}/pulled"),
        HOST,
        Some("1"),
        JSON,
        Some("https://evil.example"),
        r#"{"pulled":true}"#,
    );
    assert_eq!(w.status, 403);
    assert_eq!(
        card(&a, id)["pulled"],
        false,
        "a refused write changed nothing"
    );
}

#[test]
fn posts_without_the_marker_or_json_are_refused() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    let id = first_copy(&a);
    let p = format!("/api/copy/{id}/condition");
    let b = r#"{"condition":"LP"}"#;
    assert_eq!(
        a.route("POST", &p, HOST, None, JSON, OURS, b).status,
        403,
        "no marker"
    );
    assert_eq!(
        a.route("POST", &p, HOST, Some("0"), JSON, OURS, b).status,
        403,
        "wrong marker"
    );
    assert_eq!(
        a.route("POST", &p, HOST, Some("1"), Some("text/plain"), OURS, b)
            .status,
        403,
        "form-style body"
    );
    assert_eq!(
        a.route("POST", &p, HOST, Some("1"), None, OURS, b).status,
        403,
        "no content type"
    );
    assert_eq!(
        a.route("POST", "/api/refresh", HOST, None, JSON, OURS, "{}")
            .status,
        403
    );
    assert_eq!(card(&a, id)["condition"], "unchecked");
}

#[test]
fn bad_input_is_refused() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    let id = first_copy(&a);
    let c = format!("/api/copy/{id}/condition");
    for b in [
        r#"{"condition":"Mint"}"#,
        r#"{"condition":""}"#,
        r#"{"condition":3}"#,
        r#"{}"#,
        "[]",
        "not json",
        r#"{"condition":null}"#,
    ] {
        assert_eq!(post(&a, &c, b).status, 400, "{b}");
    }
    let p = format!("/api/copy/{id}/pulled");
    for b in [r#"{"pulled":"yes"}"#, r#"{"pulled":1}"#, r#"{}"#] {
        assert_eq!(post(&a, &p, b).status, 400, "{b}");
    }
    assert_eq!(card(&a, id)["condition"], "unchecked");
    assert_eq!(card(&a, id)["pulled"], false);
    // Paths that aren't a copy id or a known field.
    for path in [
        "/api/copy/abc/condition",
        "/api/copy/-1/condition",
        "/api/copy/1/price",
        "/api/copy/1",
        "/api/copy//pulled",
        "/api/copy/+1/pulled",
    ] {
        assert_eq!(
            post(&a, path, r#"{"condition":"NM","pulled":true}"#).status,
            404,
            "{path}"
        );
    }
}

#[test]
fn unknown_copy_is_404() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    assert_eq!(
        post(&a, "/api/copy/999999/condition", r#"{"condition":"NM"}"#).status,
        404
    );
    assert_eq!(
        post(&a, "/api/copy/999999/pulled", r#"{"pulled":true}"#).status,
        404
    );
    assert_eq!(get(&a, "/api/nothing").status, 404);
    assert_eq!(
        a.route("DELETE", "/api/ledger", HOST, None, None, None, "")
            .status,
        405
    );
}

#[test]
fn condition_and_pulled_round_trip_through_the_database() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    let v = body(&get(&a, "/api/ledger"));
    assert_eq!(v["total_cards"], 14);
    assert_eq!(v["unchecked"], 14);
    let id = first_copy(&a);

    let r = post(
        &a,
        &format!("/api/copy/{id}/condition"),
        r#"{"condition":"lp"}"#,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let row = body(&r);
    assert_eq!(row["copy_id"], id, "the reply is the updated card row");
    assert_eq!(row["condition"], "LP");
    assert_eq!(
        body(&get(&a, "/api/ledger"))["unchecked"],
        13,
        "totals follow"
    );

    let r = post(&a, &format!("/api/copy/{id}/pulled"), r#"{"pulled":true}"#);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(body(&r)["pulled"], true);
    assert_eq!(body(&r)["condition"], "LP", "ticking keeps the condition");

    // Both survive closing and reopening the database file.
    drop(a);
    let reopened = App::with(Db::open(&d.0.join("ledger.db")).unwrap(), &d.0, 8785, None);
    let c = card(&reopened, id);
    assert_eq!(
        (c["condition"].as_str(), c["pulled"].as_bool()),
        (Some("LP"), Some(true))
    );

    // And back again.
    assert_eq!(
        body(&post(
            &reopened,
            &format!("/api/copy/{id}/condition"),
            r#"{"condition":"unchecked"}"#
        ))["condition"],
        "unchecked"
    );
    assert_eq!(
        body(&post(
            &reopened,
            &format!("/api/copy/{id}/pulled"),
            r#"{"pulled":false}"#
        ))["pulled"],
        false
    );
    let c = card(&reopened, id);
    assert_eq!(
        (c["condition"].as_str(), c["pulled"].as_bool()),
        (Some("unchecked"), Some(false))
    );
    // Other copies were not touched.
    let v = body(&get(&reopened, "/api/ledger"));
    assert!(v["cards"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["condition"] == "unchecked" && c["pulled"] == false));
}

#[test]
fn every_picker_condition_is_accepted() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    let id = first_copy(&a);
    for c in ["unchecked", "NM", "LP", "MP", "HP", "DMG"] {
        let r = post(
            &a,
            &format!("/api/copy/{id}/condition"),
            &json!({ "condition": c }).to_string(),
        );
        assert_eq!(r.status, 200, "{c}: {}", r.body);
        assert_eq!(body(&r)["condition"], c);
    }
}

#[test]
fn refresh_is_501_without_a_hook() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    let r = post(&a, "/api/refresh", "{}");
    assert_eq!(r.status, 501);
    assert_eq!(body(&r)["error"], "price refresh isn't wired yet");
}

#[test]
fn refresh_calls_the_hook_one_at_a_time() {
    let d = TempDir::new();
    let seen = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let s2 = Arc::clone(&seen);
    let hook: RefreshHook = Arc::new(move |dir: &Path| {
        s2.lock().unwrap().push(dir.to_path_buf());
        Ok("12 cards checked, 3 prices updated".into())
    });
    let a = app_with(&d, Some(hook));
    let r = post(&a, "/api/refresh", "{}");
    assert_eq!(r.status, 200);
    assert_eq!(body(&r)["report"], "12 cards checked, 3 prices updated");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![d.0.clone()],
        "the hook gets the data folder"
    );

    let held = a.refreshing.lock().unwrap();
    assert_eq!(
        post(&a, "/api/refresh", "{}").status,
        409,
        "a second refresh waits its turn"
    );
    drop(held);

    let failing: RefreshHook = Arc::new(|_: &Path| Err("TCGdex didn't answer".into()));
    let b = App::with(Db::open_in_memory().unwrap(), &d.0, 8785, Some(failing));
    let r = post(&b, "/api/refresh", "{}");
    assert_eq!(
        (r.status, body(&r)["error"].as_str()),
        (502, Some("TCGdex didn't answer"))
    );
}

#[test]
fn csp_and_safety_headers_are_set() {
    let d = TempDir::new();
    let a = app_with(&d, None);
    let h = a.security_headers();
    let csp = &h
        .iter()
        .find(|(k, _)| *k == "Content-Security-Policy")
        .expect("CSP header")
        .1;
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("script-src 'self'"));
    assert!(csp.contains("img-src 'self' https://assets.tcgdex.net;"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(!csp.contains("unsafe"));
    assert!(h
        .iter()
        .any(|(k, v)| *k == "X-Content-Type-Options" && v == "nosniff"));
    assert!(h
        .iter()
        .any(|(k, v)| *k == "X-Frame-Options" && v == "DENY"));
}

#[test]
fn the_page_loads_nothing_from_outside_and_never_uses_inner_html() {
    for (name, src) in [
        ("index.html", INDEX),
        ("app.js", APP_JS),
        ("style.css", STYLE),
    ] {
        assert!(
            !src.contains("http://") && !src.contains("@import"),
            "{name} loads something from outside"
        );
        assert!(
            !src.contains("innerHTML")
                && !src.contains("insertAdjacentHTML")
                && !src.contains("document.write"),
            "{name} builds markup from text"
        );
    }
    assert!(!INDEX.contains("https://") && !STYLE.contains("https://"));
    // app.js may name the image host (to check card image URLs), nothing else.
    assert!(APP_JS
        .match_indices("https://")
        .all(|(i, _)| APP_JS[i..].starts_with(IMAGE_HOST)));
    assert!(APP_JS.contains(MARKER));
    assert!(INDEX.contains(r#"src="/app.js""#) && INDEX.contains(r#"href="/style.css""#));
    let d = TempDir::new();
    let a = app_with(&d, None);
    assert_eq!(
        get(&a, "/app.js").content_type,
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        get(&a, "/style.css").content_type,
        "text/css; charset=utf-8"
    );
}

const FAKE_KEY: &str = "test-key-0000abcd";

fn with_key(dir: &TempDir, k: ConditionKey) -> App {
    let hook: KeyStatusHook = Arc::new(move || k.clone());
    app_with(dir, None).with_key_status(Some(hook))
}

#[test]
fn prices_tile_shows_condition_prices_off_without_a_key() {
    let d = TempDir::new();
    let j = &body(&get(&app_with(&d, None), "/api/ledger"))["justtcg"];
    assert_eq!(j, &json!({ "on": false, "ends": null, "problem": null }));
    let a = with_key(
        &d,
        ConditionKey {
            on: false,
            ends: None,
            problem: Some("can't read the saved key".into()),
        },
    );
    assert_eq!(
        body(&get(&a, "/api/ledger"))["justtcg"]["problem"],
        "can't read the saved key"
    );
}

#[test]
fn prices_tile_gets_only_the_last_four_characters() {
    let d = TempDir::new();
    let a = with_key(
        &d,
        ConditionKey {
            on: true,
            ends: Some("abcd".into()),
            problem: None,
        },
    );
    let r = get(&a, "/api/ledger");
    assert_eq!(
        body(&r)["justtcg"],
        json!({ "on": true, "ends": "abcd", "problem": null })
    );
    // Even a hook that hands over the whole key gets cut to four characters.
    let a = with_key(
        &d,
        ConditionKey {
            on: true,
            ends: Some(FAKE_KEY.into()),
            problem: None,
        },
    );
    let r = get(&a, "/api/ledger");
    assert_eq!(body(&r)["justtcg"]["ends"], "abcd");
    assert!(
        !r.body.contains(FAKE_KEY) && !r.body.contains("0000abcd"),
        "the key reached the window"
    );
    // And a "problem" can't carry it while on.
    let a = with_key(
        &d,
        ConditionKey {
            on: true,
            ends: None,
            problem: Some(FAKE_KEY.into()),
        },
    );
    assert!(!get(&a, "/api/ledger").body.contains(FAKE_KEY));
}

#[test]
fn the_window_cannot_set_the_key() {
    let d = TempDir::new();
    let a = with_key(
        &d,
        ConditionKey {
            on: false,
            ends: None,
            problem: None,
        },
    );
    let b = format!("{{\"key\":\"{FAKE_KEY}\"}}");
    for p in [
        "/api/justtcg-key",
        "/api/justtcg",
        "/api/key",
        "/api/settings",
        "/api/justtcg-key/set",
    ] {
        let r = post(&a, p, &b);
        assert_eq!(r.status, 404, "{p}: {}", r.body);
        assert!(!r.body.contains(FAKE_KEY));
    }
    assert!(
        !INDEX.contains("justtcg-key") && !APP_JS.contains("/api/justtcg"),
        "no key form or call in the page"
    );
    assert!(APP_JS.contains("Condition prices (JustTCG): "));
}

/// `--no-images`: the view the window gets carries no image URLs, even for a matched card whose
/// catalogue entry has one.
#[test]
fn no_images_removes_card_art_from_the_view() {
    use card_ledger_core::card::{Lang, Resolution, ResolvedCard};
    use card_ledger_core::model::Variant;
    let d = TempDir::new();
    let make = || {
        let mut db = Db::open(&d.0.join("ledger.db")).unwrap();
        db.import(&import::parse(SAMPLE.as_bytes()).unwrap())
            .unwrap();
        let item = db
            .items()
            .unwrap()
            .into_iter()
            .find(|i| i.product_name == "Zubat (47)")
            .unwrap();
        db.save_resolution(&Resolution {
            item_id: item.id,
            card: Some(ResolvedCard {
                lang: Lang::En,
                card_id: "xy7-47".into(),
                variant: Variant::Normal,
                canonical: "xy7-47/normal".into(),
                name: "Zubat".into(),
                dex_id: Some(41),
                rarity: None,
                category: Some("Pokemon".into()),
            }),
            reason: None,
            flags: vec![],
        })
        .unwrap();
        db.cache_put(
            "card",
            Lang::En,
            "xy7-47",
            r#"{"image":"https://assets.tcgdex.net/en/xy/xy7/47"}"#,
            "2026-11-25T00:00:00Z",
        )
        .unwrap();
        db
    };
    let with_art = App::with(make(), &d.0, 8785, None);
    let images = |a: &App| -> usize {
        body(&get(a, "/api/ledger"))["cards"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| !c["image"].is_null())
            .count()
    };
    assert_eq!(
        images(&with_art),
        1,
        "the matched card shows its art normally"
    );
    drop(with_art);
    let without = App::with(make(), &d.0, 8785, None).with_no_images(true);
    assert_eq!(images(&without), 0);
}

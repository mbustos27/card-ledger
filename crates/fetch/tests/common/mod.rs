#![allow(dead_code)]

use card_ledger_core::db::Db;
use card_ledger_fetch::{FakeClock, Response};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// 2026-11-24T22:53:02Z.
pub const T0_MS: u64 = 1_795_560_782_000;
pub const HOUR_MS: u64 = 3_600_000;
pub const DAY_MS: u64 = 24 * HOUR_MS;

pub fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

pub fn ok(name: &str) -> Response {
    Response::json(200, &fixture(name))
}

pub fn not_found() -> Response {
    Response::json(404, &fixture("en_cards_dc1-999_404.json"))
}

pub fn clock() -> FakeClock {
    FakeClock::at(T0_MS)
}

pub fn mem_db() -> Db {
    Db::open_in_memory().unwrap()
}

/// A database file in a fresh temp folder, so a second connection can count history rows.
pub fn file_db(tag: &str) -> (Db, PathBuf) {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "card-ledger-fetch-test-{}-{tag}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ledger.db");
    (Db::open(&path).unwrap(), path)
}

pub fn count_rows(path: &PathBuf, sql: &str) -> i64 {
    let c = rusqlite::Connection::open(path).unwrap();
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

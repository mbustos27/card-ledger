//! The ledger's SQLite store.
//!
//! - `items`: one row per Collectr export line (a product in a portfolio), matched across
//!   re-imports by its key (portfolio, set, product name, number, variant, grade).
//! - `copies`: one row per physical card. Each copy carries its own condition (starting as
//!   unchecked), its "pulled" tick and its note. Re-importing never changes a copy's condition or
//!   tick and never deletes a copy; copies the new export no longer lists are reported instead.

use crate::card::{ConditionPrices, Lang, MarketPrice, Resolution};
use crate::import::{Export, ExportRow};
use crate::model::{Condition, Variant};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// 1: items, copies. 2: resolutions, catalog_cache, price_quotes, condition_prices, settings
/// (all `CREATE TABLE IF NOT EXISTS`, so a v1 file upgrades in place).
pub const SCHEMA_VERSION: i64 = 2;

pub struct Db {
    conn: Connection,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    pub rows: usize,
    pub cards: u32,
    pub new_items: usize,
    pub updated_items: usize,
    pub new_copies: u32,
    /// Items in the ledger that this export doesn't list (kept, not deleted).
    pub not_in_export: Vec<String>,
    /// Items whose export quantity is below the copies the ledger holds: (item, ledger, export).
    pub fewer_in_export: Vec<(String, u32, u32)>,
    /// Same key on two export lines (merged by adding quantities).
    pub merged_lines: Vec<(usize, usize)>,
    /// Lines that look corrected in Collectr: an item this export dropped and a new item with the
    /// same portfolio, set and number. Nothing is moved; `Db::carry_over` does it on request.
    pub corrections: Vec<Correction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub old_item: i64,
    pub old_label: String,
    pub new_item: i64,
    pub new_label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: i64,
    pub portfolio: String,
    pub category: String,
    pub set_name: String,
    pub product_name: String,
    pub card_number: String,
    pub rarity: String,
    pub variant: Variant,
    pub grade: String,
    pub collectr_condition: String,
    pub collectr_price: Option<f64>,
    pub collectr_price_date: Option<String>,
    pub export_quantity: u32,
    pub in_last_export: bool,
}

impl Item {
    pub fn label(&self) -> String {
        format!(
            "{} {} #{} ({})",
            self.set_name, self.product_name, self.card_number, self.variant
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Copy {
    pub id: i64,
    pub item_id: i64,
    pub copy_no: u32,
    pub condition: Condition,
    pub pulled: bool,
    pub note: String,
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> rusqlite::Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Db> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS items (
                 id INTEGER PRIMARY KEY,
                 portfolio TEXT NOT NULL,
                 category TEXT NOT NULL,
                 set_name TEXT NOT NULL,
                 product_name TEXT NOT NULL,
                 card_number TEXT NOT NULL,
                 rarity TEXT NOT NULL,
                 variant TEXT NOT NULL,
                 grade TEXT NOT NULL,
                 collectr_condition TEXT NOT NULL,
                 collectr_price REAL,
                 collectr_price_date TEXT,
                 export_quantity INTEGER NOT NULL,
                 in_last_export INTEGER NOT NULL DEFAULT 1,
                 date_added TEXT NOT NULL,
                 UNIQUE (portfolio, set_name, product_name, card_number, variant, grade)
             );
             CREATE TABLE IF NOT EXISTS copies (
                 id INTEGER PRIMARY KEY,
                 item_id INTEGER NOT NULL REFERENCES items(id),
                 copy_no INTEGER NOT NULL,
                 condition TEXT NOT NULL DEFAULT 'unchecked',
                 pulled INTEGER NOT NULL DEFAULT 0,
                 note TEXT NOT NULL DEFAULT '',
                 UNIQUE (item_id, copy_no)
             );
             CREATE TABLE IF NOT EXISTS resolutions (
                 item_id INTEGER PRIMARY KEY REFERENCES items(id),
                 resolved INTEGER NOT NULL,
                 json TEXT NOT NULL,
                 resolved_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS catalog_cache (
                 kind TEXT NOT NULL,
                 lang TEXT NOT NULL,
                 key TEXT NOT NULL,
                 json TEXT NOT NULL,
                 fetched_at TEXT NOT NULL,
                 PRIMARY KEY (kind, lang, key)
             );
             CREATE TABLE IF NOT EXISTS price_quotes (
                 lang TEXT NOT NULL,
                 card_id TEXT NOT NULL,
                 source TEXT NOT NULL,
                 bucket TEXT NOT NULL,
                 json TEXT NOT NULL,
                 fetched_at TEXT NOT NULL,
                 PRIMARY KEY (lang, card_id, source, bucket, fetched_at)
             );
             CREATE TABLE IF NOT EXISTS condition_prices (
                 lang TEXT NOT NULL,
                 card_id TEXT NOT NULL,
                 variant TEXT NOT NULL,
                 source TEXT NOT NULL,
                 json TEXT NOT NULL,
                 fetched_at TEXT NOT NULL,
                 PRIMARY KEY (lang, card_id, variant, source, fetched_at)
             );
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let have: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
                r.get(0)
            })
            .optional()?;
        match have.and_then(|v| v.parse::<i64>().ok()) {
            None => {
                conn.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema', ?1)",
                    [SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "this ledger was written by a newer card-ledger (schema {v}); update the program"
                )));
            }
            Some(v) if v < SCHEMA_VERSION => {
                conn.execute(
                    "UPDATE meta SET value=?1 WHERE key='schema'",
                    [SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(_) => {}
        }
        Ok(Db { conn })
    }

    /// Imports (or re-imports) an export in one transaction.
    pub fn import(&mut self, export: &Export) -> rusqlite::Result<ImportReport> {
        let mut report = ImportReport {
            rows: export.rows.len(),
            cards: export.card_count(),
            ..Default::default()
        };
        // Merge duplicate keys inside one export (adding quantities) before touching the store.
        let mut merged: Vec<ExportRow> = Vec::new();
        for row in &export.rows {
            if let Some(prev) = merged.iter_mut().find(|m| m.key() == row.key()) {
                report.merged_lines.push((prev.line, row.line));
                prev.quantity += row.quantity;
                prev.notes.extend(row.notes.iter().cloned());
            } else {
                merged.push(row.clone());
            }
        }

        let tx = self.conn.transaction()?;
        tx.execute("UPDATE items SET in_last_export = 0", [])?;
        let mut new_ids: Vec<i64> = Vec::new();
        for row in &merged {
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM items WHERE portfolio=?1 AND set_name=?2 AND product_name=?3
                     AND card_number=?4 AND variant=?5 AND grade=?6",
                    params![
                        row.portfolio,
                        row.set_name,
                        row.product_name,
                        row.card_number,
                        row.variant.label(),
                        row.grade
                    ],
                    |r| r.get(0),
                )
                .optional()?;
            let item_id = match existing {
                Some(id) => {
                    tx.execute(
                        "UPDATE items SET category=?2, rarity=?3, collectr_condition=?4,
                         collectr_price=?5, collectr_price_date=?6, export_quantity=?7,
                         in_last_export=1 WHERE id=?1",
                        params![
                            id,
                            row.category,
                            row.rarity,
                            row.collectr_condition,
                            row.collectr_price,
                            export.price_date,
                            row.quantity
                        ],
                    )?;
                    report.updated_items += 1;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO items (portfolio, category, set_name, product_name,
                         card_number, rarity, variant, grade, collectr_condition, collectr_price,
                         collectr_price_date, export_quantity, in_last_export, date_added)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,?13)",
                        params![
                            row.portfolio,
                            row.category,
                            row.set_name,
                            row.product_name,
                            row.card_number,
                            row.rarity,
                            row.variant.label(),
                            row.grade,
                            row.collectr_condition,
                            row.collectr_price,
                            export.price_date,
                            row.quantity,
                            row.date_added
                        ],
                    )?;
                    report.new_items += 1;
                    new_ids.push(tx.last_insert_rowid());
                    tx.last_insert_rowid()
                }
            };
            let held: u32 = tx.query_row(
                "SELECT COUNT(*) FROM copies WHERE item_id=?1",
                [item_id],
                |r| r.get(0),
            )?;
            if held > row.quantity {
                report
                    .fewer_in_export
                    .push((item_label(row), held, row.quantity));
            }
            for n in (held + 1)..=row.quantity {
                let note = row.notes.get((n - 1) as usize).cloned().unwrap_or_default();
                tx.execute(
                    "INSERT INTO copies (item_id, copy_no, condition, pulled, note)
                     VALUES (?1, ?2, 'unchecked', 0, ?3)",
                    params![item_id, n, note],
                )?;
                report.new_copies += 1;
            }
        }
        {
            let mut st = tx.prepare(
                "SELECT set_name, product_name, card_number, variant FROM items
                 WHERE in_last_export = 0 ORDER BY set_name, product_name",
            )?;
            let gone = st.query_map([], |r| {
                Ok(format!(
                    "{} {} #{} ({})",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?
                ))
            })?;
            for g in gone {
                report.not_in_export.push(g?);
            }
        }
        {
            let label = |r: &rusqlite::Row| -> rusqlite::Result<String> {
                Ok(format!(
                    "{} {} #{} ({})",
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?
                ))
            };
            let mut st = tx.prepare(
                "SELECT o.id, o.set_name, o.product_name, o.card_number, o.variant, n.id
                 FROM items o JOIN items n ON n.portfolio = o.portfolio
                   AND n.set_name = o.set_name AND n.card_number = o.card_number
                 WHERE o.in_last_export = 0 AND n.in_last_export = 1 AND n.id != o.id
                 ORDER BY o.id, n.id",
            )?;
            let pairs: Vec<(i64, String, i64)> = st
                .query_map([], |r| Ok((r.get(0)?, label(r)?, r.get(5)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (old_item, old_label, new_item) in pairs {
                if !new_ids.contains(&new_item) {
                    continue;
                }
                let new_label: String = tx.query_row(
                    "SELECT set_name || ' ' || product_name || ' #' || card_number || ' (' || variant || ')'
                     FROM items WHERE id=?1",
                    [new_item],
                    |r| r.get(0),
                )?;
                report.corrections.push(Correction {
                    old_item,
                    old_label,
                    new_item,
                    new_label,
                });
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_import_price_date', ?1)",
            [export.price_date.clone().unwrap_or_default()],
        )?;
        tx.commit()?;
        Ok(report)
    }

    pub fn items(&self) -> rusqlite::Result<Vec<Item>> {
        let mut st = self.conn.prepare(
            "SELECT id, portfolio, category, set_name, product_name, card_number, rarity, variant,
             grade, collectr_condition, collectr_price, collectr_price_date, export_quantity,
             in_last_export FROM items ORDER BY set_name, card_number, product_name, id",
        )?;
        let rows = st.query_map([], |r| {
            let v: String = r.get(7)?;
            Ok(Item {
                id: r.get(0)?,
                portfolio: r.get(1)?,
                category: r.get(2)?,
                set_name: r.get(3)?,
                product_name: r.get(4)?,
                card_number: r.get(5)?,
                rarity: r.get(6)?,
                variant: Variant::parse(&v).ok_or_else(|| bad_value("variant", &v))?,
                grade: r.get(8)?,
                collectr_condition: r.get(9)?,
                collectr_price: r.get(10)?,
                collectr_price_date: r.get(11)?,
                export_quantity: r.get(12)?,
                in_last_export: r.get::<_, i64>(13)? != 0,
            })
        })?;
        rows.collect()
    }

    pub fn copies(&self) -> rusqlite::Result<Vec<Copy>> {
        let mut st = self.conn.prepare(
            "SELECT id, item_id, copy_no, condition, pulled, note FROM copies
             ORDER BY item_id, copy_no",
        )?;
        let rows = st.query_map([], |r| {
            let c: String = r.get(3)?;
            Ok(Copy {
                id: r.get(0)?,
                item_id: r.get(1)?,
                copy_no: r.get(2)?,
                condition: Condition::parse(&c).ok_or_else(|| bad_value("condition", &c))?,
                pulled: r.get::<_, i64>(4)? != 0,
                note: r.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Sets one copy's condition. Returns false when no such copy exists.
    pub fn set_condition(&self, copy_id: i64, c: Condition) -> rusqlite::Result<bool> {
        Ok(self.conn.execute(
            "UPDATE copies SET condition=?2 WHERE id=?1",
            params![copy_id, c.code()],
        )? == 1)
    }

    /// Sets one copy's "pulled" tick. Returns false when no such copy exists.
    pub fn set_pulled(&self, copy_id: i64, pulled: bool) -> rusqlite::Result<bool> {
        Ok(self.conn.execute(
            "UPDATE copies SET pulled=?2 WHERE id=?1",
            params![copy_id, pulled as i64],
        )? == 1)
    }
}

/// A stored price line with when we fetched it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredQuote {
    pub quote: MarketPrice,
    pub fetched_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredConditionPrices {
    pub prices: ConditionPrices,
    pub fetched_at: String,
}

impl Db {
    // ---- catalogue cache (used by the network crate) ----

    /// A cached catalogue answer: (json, fetched_at). `kind` is e.g. "card" or "set".
    pub fn cache_get(
        &self,
        kind: &str,
        lang: Lang,
        key: &str,
    ) -> rusqlite::Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT json, fetched_at FROM catalog_cache WHERE kind=?1 AND lang=?2 AND key=?3",
                params![kind, lang.code(), key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    pub fn cache_put(
        &self,
        kind: &str,
        lang: Lang,
        key: &str,
        json: &str,
        fetched_at: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO catalog_cache (kind, lang, key, json, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind, lang.code(), key, json, fetched_at],
        )?;
        Ok(())
    }

    // ---- resolutions ----

    pub fn save_resolution(&self, r: &Resolution) -> rusqlite::Result<()> {
        let json = serde_json::to_string(r).map_err(|e| bad_value("resolution", &e.to_string()))?;
        self.conn.execute(
            "INSERT OR REPLACE INTO resolutions (item_id, resolved, json, resolved_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                r.item_id,
                r.card.is_some() as i64,
                json,
                crate::time::now_iso()
            ],
        )?;
        Ok(())
    }

    pub fn resolutions(&self) -> rusqlite::Result<Vec<Resolution>> {
        let mut st = self
            .conn
            .prepare("SELECT json FROM resolutions ORDER BY item_id")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for j in rows {
            let j = j?;
            out.push(
                serde_json::from_str(&j).map_err(|e| bad_value("resolution", &e.to_string()))?,
            );
        }
        Ok(out)
    }

    // ---- prices (history is kept: one row per fetch) ----

    pub fn save_quotes(
        &self,
        lang: Lang,
        card_id: &str,
        quotes: &[MarketPrice],
        fetched_at: &str,
    ) -> rusqlite::Result<()> {
        for q in quotes {
            let json = serde_json::to_string(q).map_err(|e| bad_value("quote", &e.to_string()))?;
            self.conn.execute(
                "INSERT OR REPLACE INTO price_quotes (lang, card_id, source, bucket, json, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![lang.code(), card_id, q.source, q.bucket, json, fetched_at],
            )?;
        }
        Ok(())
    }

    /// The newest quote per (source, bucket) for a card.
    pub fn latest_quotes(&self, lang: Lang, card_id: &str) -> rusqlite::Result<Vec<StoredQuote>> {
        let mut st = self.conn.prepare(
            "SELECT json, fetched_at FROM price_quotes p WHERE lang=?1 AND card_id=?2
             AND fetched_at = (SELECT MAX(fetched_at) FROM price_quotes q WHERE q.lang=p.lang
                AND q.card_id=p.card_id AND q.source=p.source AND q.bucket=p.bucket)
             ORDER BY source, bucket",
        )?;
        let rows = st.query_map(params![lang.code(), card_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (j, at) = row?;
            out.push(StoredQuote {
                quote: serde_json::from_str(&j).map_err(|e| bad_value("quote", &e.to_string()))?,
                fetched_at: at,
            });
        }
        Ok(out)
    }

    /// When prices for this card were last fetched (any source).
    pub fn last_price_fetch(&self, lang: Lang, card_id: &str) -> rusqlite::Result<Option<String>> {
        self.conn.query_row(
            "SELECT MAX(fetched_at) FROM price_quotes WHERE lang=?1 AND card_id=?2",
            params![lang.code(), card_id],
            |r| r.get(0),
        )
    }

    pub fn save_condition_prices(
        &self,
        lang: Lang,
        card_id: &str,
        p: &ConditionPrices,
        fetched_at: &str,
    ) -> rusqlite::Result<()> {
        let json =
            serde_json::to_string(p).map_err(|e| bad_value("condition prices", &e.to_string()))?;
        self.conn.execute(
            "INSERT OR REPLACE INTO condition_prices (lang, card_id, variant, source, json, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![lang.code(), card_id, p.variant.label(), p.source, json, fetched_at],
        )?;
        Ok(())
    }

    pub fn latest_condition_prices(
        &self,
        lang: Lang,
        card_id: &str,
        variant: Variant,
    ) -> rusqlite::Result<Option<StoredConditionPrices>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT json, fetched_at FROM condition_prices WHERE lang=?1 AND card_id=?2
                 AND variant=?3 ORDER BY fetched_at DESC LIMIT 1",
                params![lang.code(), card_id, variant.label()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(j, at)| {
            Ok(StoredConditionPrices {
                prices: serde_json::from_str(&j)
                    .map_err(|e| bad_value("condition prices", &e.to_string()))?,
                fetched_at: at,
            })
        })
        .transpose()
    }

    // ---- settings ----

    pub fn setting(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// One copy by id.
    pub fn copy(&self, copy_id: i64) -> rusqlite::Result<Option<Copy>> {
        Ok(self.copies()?.into_iter().find(|c| c.id == copy_id))
    }

    /// Moves what the user recorded (condition, pulled tick, note) from the copies of an item the
    /// export dropped onto the copies of the item that replaced it, pairing them by copy number.
    /// Only copies of `to` that are still untouched (unchecked, not pulled, no note) receive
    /// anything; the old copies are reset to untouched so nothing is counted twice. Returns how
    /// many copies were carried over. Refuses unless `from` is out of the export and `to` is in it.
    pub fn carry_over(&mut self, from: i64, to: i64) -> Result<u32, String> {
        let state = |id: i64| -> Result<Option<bool>, String> {
            self.conn
                .query_row("SELECT in_last_export FROM items WHERE id=?1", [id], |r| {
                    Ok(r.get::<_, i64>(0)? != 0)
                })
                .optional()
                .map_err(|e| e.to_string())
        };
        match (state(from)?, state(to)?) {
            (None, _) => return Err(format!("no card line with item id {from}")),
            (_, None) => return Err(format!("no card line with item id {to}")),
            (Some(true), _) => return Err(format!("item {from} is still in the latest export")),
            (_, Some(false)) => return Err(format!("item {to} is not in the latest export")),
            _ => {}
        }
        let copies = self.copies().map_err(|e| e.to_string())?;
        let olds: Vec<&Copy> = copies.iter().filter(|c| c.item_id == from).collect();
        let news: Vec<&Copy> = copies.iter().filter(|c| c.item_id == to).collect();
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let mut moved = 0;
        for (o, n) in olds.iter().zip(news.iter()) {
            let untouched_new =
                n.condition == Condition::Unchecked && !n.pulled && n.note.is_empty();
            let touched_old = o.condition != Condition::Unchecked || o.pulled || !o.note.is_empty();
            if !(untouched_new && touched_old) {
                continue;
            }
            tx.execute(
                "UPDATE copies SET condition=?2, pulled=?3, note=?4 WHERE id=?1",
                params![n.id, o.condition.code(), o.pulled as i64, o.note],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE copies SET condition='unchecked', pulled=0, note='' WHERE id=?1",
                [o.id],
            )
            .map_err(|e| e.to_string())?;
            moved += 1;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(moved)
    }
}

fn item_label(row: &ExportRow) -> String {
    format!(
        "{} {} #{} ({})",
        row.set_name, row.product_name, row.card_number, row.variant
    )
}

fn bad_value(what: &str, v: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(format!("stored {what} {v:?} is not recognised"))
}

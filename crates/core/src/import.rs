//! Reads a Collectr portfolio export (CSV).
//!
//! What we keep from Collectr and what we don't:
//! - Set, product name, number, rarity, variant, quantity and notes are kept as Collectr wrote
//!   them. Identity checks happen later (`identity`), never here.
//! - "Card Condition" is stored only as `collectr_condition` for reference. Collectr marks every
//!   card Near Mint by default, so the ledger's own condition always starts as unchecked.
//! - "Market Price" is kept as Collectr's NM figure with the date from the column header, and is
//!   only ever shown as "Collectr (NM)". A price of 0 means Collectr had none.
//! - Notes: for a row with quantity N, Collectr writes one note per copy separated by ";"
//!   (`"; "` for two blank notes), so notes are split per copy.

use crate::model::Variant;
use std::collections::HashMap;
use std::fmt;
use std::io::Read;

/// The columns a Collectr export must have (any order; extra columns are ignored).
pub const REQUIRED: [&str; 16] = [
    "Portfolio Name",
    "Category",
    "Set",
    "Product Name",
    "Card Number",
    "Rarity",
    "Variance",
    "Grade",
    "Card Condition",
    "Average Cost Paid",
    "Quantity",
    "Market Price",
    "Price Override",
    "Watchlist",
    "Date Added",
    "Notes",
];

/// The largest quantity accepted on one row (a typo guard, not a collection limit).
pub const MAX_QUANTITY: u32 = 999;

#[derive(Debug, Clone, PartialEq)]
pub struct ExportRow {
    /// 1-based line number in the file (the header is line 1).
    pub line: usize,
    pub portfolio: String,
    pub category: String,
    pub set_name: String,
    pub product_name: String,
    pub card_number: String,
    pub rarity: String,
    pub variant: Variant,
    pub grade: String,
    pub collectr_condition: String,
    pub avg_cost: Option<f64>,
    pub quantity: u32,
    /// Collectr's market price (NM) for one copy; `None` when Collectr had no price (0 or blank).
    pub collectr_price: Option<f64>,
    pub price_override: Option<f64>,
    pub watchlist: bool,
    pub date_added: String,
    /// One note per copy, length == quantity.
    pub notes: Vec<String>,
}

impl ExportRow {
    /// The key one export line is matched on across re-imports.
    pub fn key(&self) -> RowKey {
        RowKey {
            portfolio: self.portfolio.clone(),
            set_name: self.set_name.clone(),
            product_name: self.product_name.clone(),
            card_number: self.card_number.clone(),
            variant: self.variant,
            grade: self.grade.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowKey {
    pub portfolio: String,
    pub set_name: String,
    pub product_name: String,
    pub card_number: String,
    pub variant: Variant,
    pub grade: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    /// The date in the "Market Price (As of YYYY-MM-DD)" header, if present.
    pub price_date: Option<String>,
    pub rows: Vec<ExportRow>,
}

impl Export {
    /// Number of physical cards (sum of quantities).
    pub fn card_count(&self) -> u32 {
        self.rows.iter().map(|r| r.quantity).sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportError {
    Csv(String),
    MissingColumns(Vec<String>),
    /// Every bad line, so the user can fix them in one pass.
    BadRows(Vec<(usize, String)>),
    Empty,
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Csv(e) => write!(f, "not a readable CSV file: {e}"),
            ImportError::MissingColumns(c) => write!(
                f,
                "this doesn't look like a Collectr export; missing column(s): {}",
                c.join(", ")
            ),
            ImportError::BadRows(rows) => {
                write!(f, "{} line(s) could not be read:", rows.len())?;
                for (line, why) in rows {
                    write!(f, "\n  line {line}: {why}")?;
                }
                Ok(())
            }
            ImportError::Empty => write!(f, "the export has a header but no cards"),
        }
    }
}

impl std::error::Error for ImportError {}

/// Parses a Collectr export from any reader.
pub fn parse<R: Read>(reader: R) -> Result<Export, ImportError> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .trim(csv::Trim::None)
        .from_reader(reader);
    let headers = rdr
        .headers()
        .map_err(|e| ImportError::Csv(e.to_string()))?
        .clone();

    // Map each required column to its index. "Market Price" carries a date suffix.
    let mut idx: HashMap<&str, usize> = HashMap::new();
    let mut price_date = None;
    for (i, h) in headers.iter().enumerate() {
        let h = h.trim().trim_start_matches('\u{feff}');
        for want in REQUIRED {
            let hit = if want == "Market Price" {
                h == want || h.starts_with("Market Price (")
            } else {
                h == want
            };
            if hit && !idx.contains_key(want) {
                idx.insert(want, i);
                if want == "Market Price" {
                    price_date = header_date(h);
                }
            }
        }
    }
    let missing: Vec<String> = REQUIRED
        .iter()
        .filter(|c| !idx.contains_key(*c))
        .map(|c| c.to_string())
        .collect();
    if !missing.is_empty() {
        return Err(ImportError::MissingColumns(missing));
    }

    let mut rows = Vec::new();
    let mut bad = Vec::new();
    for (n, rec) in rdr.records().enumerate() {
        let line = n + 2;
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                bad.push((line, format!("unreadable ({e})")));
                continue;
            }
        };
        let get = |c: &str| rec.get(idx[c]).unwrap_or("").trim().to_string();
        if rec.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        match row_from(line, &get) {
            Ok(r) => rows.push(r),
            Err(why) => bad.push((line, why)),
        }
    }
    if !bad.is_empty() {
        return Err(ImportError::BadRows(bad));
    }
    if rows.is_empty() {
        return Err(ImportError::Empty);
    }
    Ok(Export { price_date, rows })
}

fn row_from(line: usize, get: &dyn Fn(&str) -> String) -> Result<ExportRow, String> {
    let product_name = get("Product Name");
    if product_name.is_empty() {
        return Err("no product name".into());
    }
    let set_name = get("Set");
    if set_name.is_empty() {
        return Err(format!("{product_name}: no set name"));
    }
    let variant_raw = get("Variance");
    let variant = Variant::parse(&variant_raw)
        .ok_or_else(|| format!("{product_name}: unknown variant {variant_raw:?}"))?;
    let qty_raw = get("Quantity");
    let quantity: u32 = qty_raw
        .parse()
        .map_err(|_| format!("{product_name}: quantity {qty_raw:?} is not a whole number"))?;
    if quantity == 0 || quantity > MAX_QUANTITY {
        return Err(format!(
            "{product_name}: quantity {quantity} is outside 1..={MAX_QUANTITY}"
        ));
    }
    let collectr_price = money(&get("Market Price"), "market price", &product_name)?;
    let avg_cost = money(&get("Average Cost Paid"), "cost paid", &product_name)?;
    let price_override = money(&get("Price Override"), "price override", &product_name)?;
    let watchlist = matches!(
        get("Watchlist").to_ascii_lowercase().as_str(),
        "true" | "yes" | "1"
    );
    let notes = split_notes(&get("Notes"), quantity);
    Ok(ExportRow {
        line,
        portfolio: get("Portfolio Name"),
        category: get("Category"),
        set_name,
        product_name,
        card_number: get("Card Number"),
        rarity: get("Rarity"),
        variant,
        grade: get("Grade"),
        collectr_condition: get("Card Condition"),
        avg_cost,
        quantity,
        collectr_price,
        price_override,
        watchlist,
        date_added: get("Date Added"),
        notes,
    })
}

/// A money cell: blank or 0 is "none"; negatives, NaN and infinities are errors.
fn money(raw: &str, what: &str, name: &str) -> Result<Option<f64>, String> {
    let t = raw.trim().trim_start_matches('$').replace(',', "");
    if t.is_empty() {
        return Ok(None);
    }
    let v: f64 = t
        .parse()
        .map_err(|_| format!("{name}: {what} {raw:?} is not a number"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("{name}: {what} {raw:?} is not a usable amount"));
    }
    Ok(if v == 0.0 { None } else { Some(v) })
}

/// Collectr writes one note per copy separated by ';'. Always returns exactly `quantity` notes;
/// with a single copy the whole cell is that copy's note (so a ';' inside it is kept).
pub fn split_notes(raw: &str, quantity: u32) -> Vec<String> {
    let q = quantity as usize;
    if q <= 1 {
        return vec![raw.trim().to_string(); q];
    }
    let mut parts: Vec<String> = raw.split(';').map(|p| p.trim().to_string()).collect();
    if parts.len() > q {
        // More separators than copies: keep the extra text on the last copy rather than lose it.
        let tail = parts.split_off(q - 1).join("; ");
        parts.push(
            tail.trim_matches(|c: char| c == ';' || c.is_whitespace())
                .to_string(),
        );
    }
    parts.resize(q, String::new());
    parts
}

fn header_date(h: &str) -> Option<String> {
    let inner = h.split_once("(As of ")?.1.strip_suffix(')')?.trim();
    let ok = inner.len() == 10
        && inner.char_indices().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }
        });
    ok.then(|| inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Portfolio Name,Category,Set,Product Name,Card Number,Rarity,Variance,Grade,Card Condition,Average Cost Paid,Quantity,Market Price (As of 2026-11-25),Price Override,Watchlist,Date Added,Notes";

    fn one(line: &str) -> Result<Export, ImportError> {
        parse(format!("{HEADER}\n{line}\n").as_bytes())
    }

    #[test]
    fn reads_price_date_from_header() {
        let e = one("Main,Pokemon,Example Set,Lickitung,48,Rare,Normal,Ungraded,Near Mint,0.0000,1,3.5,0,false,2026-11-25,").unwrap();
        assert_eq!(e.price_date.as_deref(), Some("2026-11-25"));
        assert_eq!(e.rows[0].collectr_price, Some(3.5));
        assert_eq!(e.rows[0].avg_cost, None);
        assert_eq!(e.rows[0].line, 2);
    }

    #[test]
    fn zero_price_means_no_price() {
        let e = one("Main,Pokemon,Megalo Cannon,Colress Machine (JP),070/076,Rare,1st Edition Holofoil,Ungraded,Near Mint,0.0000,1,0,0,false,2026-11-25,").unwrap();
        assert_eq!(e.rows[0].collectr_price, None);
        assert_eq!(e.rows[0].variant, Variant::FirstEditionHolofoil);
    }

    #[test]
    fn notes_split_per_copy() {
        assert_eq!(split_notes("; ; ", 3), vec!["", "", ""]);
        assert_eq!(split_notes("; ", 2), vec!["", ""]);
        assert_eq!(split_notes("dent;", 2), vec!["dent", ""]);
        assert_eq!(split_notes("a; b; c", 2), vec!["a", "b; c"]);
        assert_eq!(split_notes("x;y", 1), vec!["x;y"]);
        assert_eq!(split_notes("", 2), vec!["", ""]);
    }

    #[test]
    fn collects_every_bad_line() {
        let text = format!(
            "{HEADER}\n\
             Main,Pokemon,Example Set,Lickitung,48,Rare,Cosmos Holo,Ungraded,Near Mint,0,1,3.5,0,false,2026-11-25,\n\
             Main,Pokemon,Example Set,Lickitung,48,Rare,Normal,Ungraded,Near Mint,0,two,3.5,0,false,2026-11-25,\n\
             Main,Pokemon,Example Set,Lickitung,48,Rare,Normal,Ungraded,Near Mint,0,1,NaN,0,false,2026-11-25,\n\
             Main,Pokemon,Example Set,Lickitung,48,Rare,Normal,Ungraded,Near Mint,0,0,1,0,false,2026-11-25,\n"
        );
        match parse(text.as_bytes()) {
            Err(ImportError::BadRows(b)) => {
                assert_eq!(
                    b.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
                    vec![2, 3, 4, 5]
                );
            }
            other => panic!("expected bad rows, got {other:?}"),
        }
    }

    #[test]
    fn rejects_files_that_are_not_collectr_exports() {
        let r = parse("Name,Set,Qty\nSnorlax,Jungle,1\n".as_bytes());
        assert!(
            matches!(r, Err(ImportError::MissingColumns(c)) if c.contains(&"Variance".to_string()))
        );
    }

    #[test]
    fn header_only_is_empty() {
        assert_eq!(
            parse(format!("{HEADER}\n").as_bytes()),
            Err(ImportError::Empty)
        );
    }

    #[test]
    fn columns_in_any_order_and_bom() {
        let text = "\u{feff}Notes,Date Added,Watchlist,Price Override,Market Price (As of 2026-11-25),Quantity,Average Cost Paid,Card Condition,Grade,Variance,Rarity,Card Number,Product Name,Set,Category,Portfolio Name\n,2026-01-15,false,0,1.5,2,0,Near Mint,Ungraded,Reverse Holofoil,Common,77,Pidove,Example Set,Pokemon,Main\n";
        let e = parse(text.as_bytes()).unwrap();
        assert_eq!(e.rows[0].product_name, "Pidove");
        assert_eq!(e.rows[0].quantity, 2);
        assert_eq!(e.card_count(), 2);
    }

    #[test]
    fn quoted_commas_survive() {
        let e = one("Main,Pokemon,\"Example, Promos\",\"Kecleon, Hidden\",EX001,Promo,Holofoil,Ungraded,Near Mint,0,1,2,0,false,2026-01-15,\"ring dent, top left\"").unwrap();
        assert_eq!(e.rows[0].set_name, "Example, Promos");
        assert_eq!(e.rows[0].notes, vec!["ring dent, top left"]);
    }
}

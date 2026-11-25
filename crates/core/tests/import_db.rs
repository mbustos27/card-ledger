use card_ledger_core::db::Db;
use card_ledger_core::import::{parse, Export};
use card_ledger_core::model::{Condition, Variant};

const SAMPLE: &str = include_str!("fixtures/sample.csv");

fn sample() -> Export {
    parse(SAMPLE.as_bytes()).expect("sample parses")
}

#[test]
fn sample_imports_every_card_unchecked() {
    let e = sample();
    assert_eq!(e.rows.len(), 9);
    assert_eq!(e.card_count(), 14);
    let mut db = Db::open_in_memory().unwrap();
    let r = db.import(&e).unwrap();
    assert_eq!((r.new_items, r.new_copies), (9, 14));
    let copies = db.copies().unwrap();
    assert_eq!(copies.len(), 14);
    // Collectr said Near Mint for every row; the ledger must not believe it.
    assert!(copies
        .iter()
        .all(|c| c.condition == Condition::Unchecked && !c.pulled));
    let items = db.items().unwrap();
    assert!(items.iter().all(|i| i.collectr_condition == "Near Mint"));
    let staryus: Vec<_> = items
        .iter()
        .filter(|i| i.product_name == "Staryu")
        .collect();
    assert_eq!(staryus.len(), 2, "normal and reverse are different items");
    let colress = items
        .iter()
        .find(|i| i.product_name.starts_with("Colress Machine"))
        .unwrap();
    assert_eq!(colress.collectr_price, None);
    assert_eq!(colress.variant, Variant::FirstEditionHolofoil);
    assert_eq!(colress.collectr_price_date.as_deref(), Some("2026-01-15"));
}

#[test]
fn reimport_keeps_conditions_and_ticks_and_never_deletes() {
    let mut db = Db::open_in_memory().unwrap();
    db.import(&sample()).unwrap();
    let oddish = db
        .items()
        .unwrap()
        .into_iter()
        .find(|i| i.product_name == "Oddish")
        .unwrap();
    let first = db
        .copies()
        .unwrap()
        .into_iter()
        .find(|c| c.item_id == oddish.id && c.copy_no == 1)
        .unwrap();
    assert!(db
        .set_condition(first.id, Condition::LightlyPlayed)
        .unwrap());
    assert!(db.set_pulled(first.id, true).unwrap());
    assert!(!db.set_condition(999_999, Condition::NearMint).unwrap());

    // Second export: Oddish drops to 1 copy, Zubat gone, Skyla up to 4, new Lickitung.
    let text = SAMPLE
        .replace("Oddish,9,Common,Reverse Holofoil,Ungraded,Near Mint,0.0000,4,3.00", "Oddish,9,Common,Reverse Holofoil,Ungraded,Near Mint,0.0000,1,1.10")
        .replace("Main,Pokemon,Ancient Origins,Zubat (47),47,Common,Normal,Ungraded,Near Mint,0.0000,1,0.10,0,false,2026-01-15,\n", "")
        .replace("Skyla,RC24,Uncommon,Holofoil,Ungraded,Near Mint,0.0000,3,0.75,0,false,2026-01-15,; ; ", "Skyla,RC24,Uncommon,Holofoil,Ungraded,Near Mint,0.0000,4,0.75,0,false,2026-01-15,; ; ; ")
        + "Main,Pokemon,Example Set,Lickitung,48,Rare,Holofoil,Ungraded,Near Mint,0.0000,1,12,0,false,2026-11-26,\n";
    let r = db.import(&parse(text.as_bytes()).unwrap()).unwrap();
    assert_eq!(r.new_items, 1);
    assert_eq!(r.new_copies, 2, "one more Skyla, one Lickitung");
    assert_eq!(r.not_in_export.len(), 1);
    assert!(r.not_in_export[0].contains("Zubat"));
    assert_eq!(r.fewer_in_export.len(), 1);
    assert_eq!((r.fewer_in_export[0].1, r.fewer_in_export[0].2), (4, 1));

    let copies = db.copies().unwrap();
    assert_eq!(copies.len(), 16, "nothing deleted");
    let again = copies.iter().find(|c| c.id == first.id).unwrap();
    assert_eq!(again.condition, Condition::LightlyPlayed);
    assert!(again.pulled);
    let items = db.items().unwrap();
    let t = items.iter().find(|i| i.product_name == "Oddish").unwrap();
    assert_eq!(t.collectr_price, Some(1.10));
    assert!(
        !items
            .iter()
            .find(|i| i.product_name.starts_with("Zubat"))
            .unwrap()
            .in_last_export
    );
}

#[test]
fn duplicate_lines_in_one_export_merge() {
    let line = "Main,Pokemon,Example Set,Lickitung,48,Rare,Holofoil,Ungraded,Near Mint,0.0000,1,12,0,false,2026-11-26,";
    let text = format!("{}\n{line}\n{line}\n", SAMPLE.lines().next().unwrap());
    let mut db = Db::open_in_memory().unwrap();
    let r = db.import(&parse(text.as_bytes()).unwrap()).unwrap();
    assert_eq!(r.merged_lines, vec![(2, 3)]);
    assert_eq!(db.copies().unwrap().len(), 2);
}

#[test]
fn reopening_a_file_keeps_the_ledger() {
    let dir = std::env::temp_dir().join(format!("card-ledger-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ledger.db");
    {
        let mut db = Db::open(&path).unwrap();
        db.import(&sample()).unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.copies().unwrap().len(), 14);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_v1_ledger_upgrades_in_place_and_a_newer_one_is_refused() {
    let dir = std::env::temp_dir().join(format!("card-ledger-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ledger.db");
    {
        // A schema-1 file: items and copies only.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES ('schema', '1');
             CREATE TABLE items (id INTEGER PRIMARY KEY, portfolio TEXT NOT NULL, category TEXT NOT NULL,
               set_name TEXT NOT NULL, product_name TEXT NOT NULL, card_number TEXT NOT NULL,
               rarity TEXT NOT NULL, variant TEXT NOT NULL, grade TEXT NOT NULL,
               collectr_condition TEXT NOT NULL, collectr_price REAL, collectr_price_date TEXT,
               export_quantity INTEGER NOT NULL, in_last_export INTEGER NOT NULL DEFAULT 1,
               date_added TEXT NOT NULL,
               UNIQUE (portfolio, set_name, product_name, card_number, variant, grade));
             CREATE TABLE copies (id INTEGER PRIMARY KEY, item_id INTEGER NOT NULL REFERENCES items(id),
               copy_no INTEGER NOT NULL, condition TEXT NOT NULL DEFAULT 'unchecked',
               pulled INTEGER NOT NULL DEFAULT 0, note TEXT NOT NULL DEFAULT '', UNIQUE (item_id, copy_no));
             INSERT INTO items VALUES (1,'Main','Pokemon','Example Set','Lickitung','48','Rare','Holofoil','Ungraded','Near Mint',12.0,'2026-11-25',1,1,'2026-11-25');
             INSERT INTO copies VALUES (1,1,1,'LP',1,'corner');",
        )
        .unwrap();
    }
    {
        let db = Db::open(&path).unwrap();
        let c = db.copies().unwrap();
        assert_eq!(
            (c.len(), c[0].condition, c[0].pulled),
            (1, Condition::LightlyPlayed, true)
        );
        db.set_setting("probe", "1").unwrap();
        assert!(db.resolutions().unwrap().is_empty());
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        let v: String = conn
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, card_ledger_core::db::SCHEMA_VERSION.to_string());
        conn.execute("UPDATE meta SET value='99' WHERE key='schema'", [])
            .unwrap();
    }
    assert!(Db::open(&path).is_err(), "a newer ledger must be refused");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The demo export shipped for the README and the release zip must stay importable.
#[test]
fn the_demo_export_imports_cleanly() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/sample-export.csv");
    let e = parse(std::fs::File::open(path).expect("demo export")).expect("demo export parses");
    assert_eq!((e.rows.len(), e.card_count()), (10, 12));
    let mut db = Db::open_in_memory().unwrap();
    assert_eq!(db.import(&e).unwrap().new_copies, 12);
}

//! `card-ledger`: the command line for the card ledger.

mod update;

use card_ledger_core::db::Db;
use card_ledger_core::import;
use card_ledger_core::ledger::{self, CardView};
use card_ledger_core::model::Condition;
use card_ledger_fetch::justtcg::{self, keystore, KeySource};
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "card-ledger: track a Pokemon card collection by real condition

  card-ledger import <export.csv> [--dry-run]
      Read a Collectr export. Every new card starts as condition \"unchecked\".
      Re-importing keeps conditions and pulled ticks and never deletes cards.
  card-ledger prices [--max-age-hours N] [--rematch]
      Match new cards to TCGdex, then refresh prices older than N hours (default 24).
      With a JustTCG key (see justtcg-key), also per-condition prices from JustTCG.
  card-ledger match [--rematch]
      Only match cards to TCGdex (card ids and flags); fetch no prices.
  card-ledger list [--pile toploader|small-binder|big-binder|box|set-aside]
                   [--unchecked] [--flagged] [--unresolved] [--check]
      One line per card: copy id, pile, condition, value, name.
  card-ledger show <copy-id>
      Everything about one card: values by condition, sources, flags, research notes.
  card-ledger condition <copy-id> <unchecked|NM|LP|MP|HP|DMG>
  card-ledger pulled <copy-id> [yes|no]
  card-ledger carry-over <old-item-id> <new-item-id>
      After a line was corrected in Collectr and re-imported: move the conditions, ticks and
      notes recorded on the old line's cards to the new line's cards (import lists the pairs).
  card-ledger summary
      Piles, totals and counts.
  card-ledger ui [--port N] [--no-open] [--no-images]
      The window on http://127.0.0.1:8785/. --no-images shows placeholders instead of card art.
  card-ledger justtcg-key set|status|clear
      Your own JustTCG API key, for per-condition prices (JustTCG's free plan is for personal,
      non-commercial use). `set` reads the key from standard input (paste it, or pipe it in)
      and saves it in Windows Credential Manager (elsewhere: justtcg.key in the data folder).
      The key is never printed: `status` shows only its last four characters. It is sent only
      to https://api.justtcg.com. CARD_LEDGER_JUSTTCG_KEY, when set, is used instead.

Data: %APPDATA%\\card-ledger\\ledger.db (CARD_LEDGER_DATA_DIR overrides).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("import") => cmd_import(&args[1..]),
        Some("summary") => cmd_summary(),
        Some("prices") => cmd_prices(&args[1..]),
        Some("match") => cmd_match(&args[1..]),
        Some("list") => cmd_list(&args[1..]),
        Some("show") => cmd_show(&args[1..]),
        Some("condition") => cmd_condition(&args[1..]),
        Some("pulled") => cmd_pulled(&args[1..]),
        Some("ui") => cmd_ui(&args[1..]),
        Some("carry-over") => cmd_carry_over(&args[1..]),
        Some("justtcg-key") => cmd_justtcg_key(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("card-ledger: unknown command {other:?}\n\n{USAGE}");
            2
        }
    };
    ExitCode::from(code)
}

fn open_db() -> Result<Db, String> {
    let dir = card_ledger_core::data_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let path = dir.join("ledger.db");
    Db::open(&path).map_err(|e| format!("can't open {}: {e}", path.display()))
}

fn cmd_import(args: &[String]) -> u8 {
    let mut path = None;
    let mut dry = false;
    for a in args {
        match a.as_str() {
            "--dry-run" => dry = true,
            p if path.is_none() && !p.starts_with("--") => path = Some(p.to_string()),
            other => {
                eprintln!("card-ledger import: unexpected {other:?}");
                return 2;
            }
        }
    }
    let Some(path) = path else {
        eprintln!("card-ledger import: give the path of a Collectr export CSV");
        return 2;
    };
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("card-ledger import: can't open {path}: {e}");
            return 1;
        }
    };
    let export = match import::parse(file) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("card-ledger import: {e}");
            return 1;
        }
    };
    println!(
        "Read {} lines, {} cards (Collectr prices as of {}).",
        export.rows.len(),
        export.card_count(),
        export.price_date.as_deref().unwrap_or("an unknown date")
    );
    if dry {
        println!("Dry run: nothing saved.");
        return 0;
    }
    let mut db = match open_db() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger import: {e}");
            return 1;
        }
    };
    match db.import(&export) {
        Ok(r) => {
            println!(
                "Saved: {} new, {} updated, {} new card copies (all \"unchecked\").",
                r.new_items, r.updated_items, r.new_copies
            );
            for (a, b) in &r.merged_lines {
                println!("  lines {a} and {b} are the same card; quantities added together");
            }
            for (label, held, now) in &r.fewer_in_export {
                println!(
                    "  {label}: the export lists {now}, the ledger keeps {held} (nothing deleted)"
                );
            }
            if !r.corrections.is_empty() {
                println!("These look corrected in Collectr (same set and number, new line). Nothing was moved:");
                for c in &r.corrections {
                    println!("  {} -> {}", c.old_label, c.new_label);
                    println!("    to keep the condition and tick you recorded: card-ledger carry-over {} {}", c.old_item, c.new_item);
                }
            }
            if !r.not_in_export.is_empty() {
                println!(
                    "{} card line(s) are in the ledger but not in this export (kept):",
                    r.not_in_export.len()
                );
                for l in &r.not_in_export {
                    println!("  {l}");
                }
            }
            0
        }
        Err(e) => {
            eprintln!("card-ledger import: nothing saved: {e}");
            1
        }
    }
}

fn data_dir() -> Result<std::path::PathBuf, String> {
    let dir = card_ledger_core::data_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn view() -> Result<ledger::LedgerView, String> {
    let db = open_db()?;
    ledger::build_view(&db).map_err(|e| e.to_string())
}

fn money(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_string(), |x| format!("${x:.2}"))
}

fn cmd_summary() -> u8 {
    let v = match view() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("card-ledger summary: {e}");
            return 1;
        }
    };
    println!(
        "{} cards: {} unchecked, {} unresolved, {} flagged.",
        v.total_cards, v.unchecked, v.unresolved, v.flagged
    );
    if card_ledger_core::pricing::Research::embedded()
        .entries
        .is_empty()
    {
        println!("Note: no research values in this build; condition values use TCGplayer prices and multipliers only.");
    }
    println!(
        "Value ${:.2} with unchecked cards at LP; ${:.2} if every unchecked card were NM (Collectr NM total ${:.2}). Prices fetched: {}.",
        v.total_value_unchecked_lp,
        v.total_value,
        v.collectr_total,
        v.prices_fetched_at.as_deref().unwrap_or("never")
    );
    println!("Piles (value with unchecked cards at LP / at NM):");
    for p in &v.piles {
        if p.pile == ledger::NOT_IN_EXPORT && p.cards == 0 {
            continue;
        }
        println!(
            "  {:<13} {:>4} cards  {:>4} pulled  ${:>9.2} / ${:>9.2}",
            p.pile, p.cards, p.pulled, p.value, p.value_if_nm
        );
    }
    0
}

fn cmd_prices(args: &[String]) -> u8 {
    let mut max_age = 24u64;
    let mut rematch = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rematch" => rematch = true,
            "--max-age-hours" => match it.next().and_then(|n| n.parse().ok()) {
                Some(n) => max_age = n,
                None => {
                    eprintln!("card-ledger prices: --max-age-hours needs a whole number");
                    return 2;
                }
            },
            other => {
                eprintln!("card-ledger prices: unexpected {other:?}");
                return 2;
            }
        }
    }
    let dir = match data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger prices: {e}");
            return 1;
        }
    };
    match update::run(&dir, max_age, rematch, true, true) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("card-ledger prices: {e}");
            1
        }
    }
}

fn cmd_match(args: &[String]) -> u8 {
    let rematch = match args {
        [] => false,
        [a] if a == "--rematch" => true,
        _ => {
            eprintln!("card-ledger match: the only option is --rematch");
            return 2;
        }
    };
    let dir = match data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger match: {e}");
            return 1;
        }
    };
    match update::run(&dir, 24, rematch, true, false) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("card-ledger match: {e}");
            1
        }
    }
}

fn cmd_list(args: &[String]) -> u8 {
    let mut pile: Option<String> = None;
    let (mut unchecked, mut flagged, mut unresolved, mut check) = (false, false, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pile" => pile = it.next().cloned(),
            "--unchecked" => unchecked = true,
            "--flagged" => flagged = true,
            "--unresolved" => unresolved = true,
            "--check" => check = true,
            other => {
                eprintln!("card-ledger list: unexpected {other:?}");
                return 2;
            }
        }
    }
    let v = match view() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("card-ledger list: {e}");
            return 1;
        }
    };
    let keep = |c: &&CardView| {
        pile.as_ref().is_none_or(|p| &c.pile == p)
            && (!unchecked || c.condition == "unchecked")
            && (!flagged || !c.flags.is_empty())
            && (!unresolved || c.unresolved_reason.is_some())
            && (!check || c.check_condition)
    };
    let mut n = 0;
    for c in v.cards.iter().filter(keep) {
        n += 1;
        let mut tail = String::new();
        if c.check_condition {
            tail.push_str("  [check condition]");
        }
        if !c.flags.is_empty() {
            tail.push_str(&format!("  [{} flag(s)]", c.flags.len()));
        }
        if let Some(r) = &c.unresolved_reason {
            tail.push_str(&format!("  [unresolved: {r}]"));
        }
        println!(
            "{:>4}  {:<12} {:<9} {:>9}  {} #{} ({}) - {}{}{}",
            c.copy_id,
            c.pile,
            c.condition,
            money(c.value),
            c.product_name,
            c.card_number,
            c.variant,
            c.set_name,
            if c.pulled { "  [pulled]" } else { "" },
            tail
        );
    }
    println!("{n} card(s).");
    0
}

fn copy_id(arg: Option<&String>, cmd: &str) -> Result<i64, u8> {
    match arg.and_then(|a| a.parse::<i64>().ok()) {
        Some(id) => Ok(id),
        None => {
            eprintln!("card-ledger {cmd}: give a copy id (the first column of `card-ledger list`)");
            Err(2)
        }
    }
}

fn cmd_show(args: &[String]) -> u8 {
    let id = match copy_id(args.first(), "show") {
        Ok(i) => i,
        Err(c) => return c,
    };
    let v = match view() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("card-ledger show: {e}");
            return 1;
        }
    };
    let Some(c) = v.cards.iter().find(|c| c.copy_id == id) else {
        eprintln!("card-ledger show: no card with copy id {id}");
        return 1;
    };
    println!(
        "{} #{} ({}) - {}",
        c.product_name, c.card_number, c.variant, c.set_name
    );
    println!(
        "  item {}, copy {} of {}; condition {}{}{}",
        c.item_id,
        c.copy_no,
        c.copies_of_item,
        c.condition,
        if c.pulled { "; pulled" } else { "" },
        if c.in_last_export {
            ""
        } else {
            "; not in the latest export"
        }
    );
    match (&c.canonical, &c.unresolved_reason) {
        (Some(k), _) => println!(
            "  card id {k} ({})",
            c.catalog_name.as_deref().unwrap_or("")
        ),
        (None, Some(r)) => println!("  unresolved: {r}"),
        _ => {}
    }
    println!(
        "  value {} ({} to {}): {}",
        money(c.value),
        money(c.value_low),
        money(c.value_high),
        c.value_basis
    );
    let names = ["NM", "LP", "MP", "HP", "DMG"];
    let row: Vec<String> = names
        .iter()
        .zip(c.by_condition.iter())
        .map(|(n, v)| format!("{n} {}", money(*v)))
        .collect();
    println!("  by condition: {}", row.join(" · "));
    if let (Some(p), Some(l)) = (c.nm_price, &c.nm_label) {
        println!("  NM {}: {l}", money(Some(p)));
    }
    println!(
        "  Collectr (NM) {} as of {}",
        money(c.collectr_price),
        c.collectr_price_date.as_deref().unwrap_or("?")
    );
    println!(
        "  pile {}: {}{}",
        c.pile,
        c.pile_reason,
        if c.check_condition {
            " (check condition)"
        } else {
            ""
        }
    );
    for f in &c.flags {
        println!("  flag {}: {}", f.code.code(), f.message);
    }
    if let Some(r) = &c.research {
        println!("  research ({} confidence): {}", r.confidence, r.note);
        println!("    check: {}", r.check);
        println!("    grading: {}", r.grade);
    }
    0
}

fn cmd_condition(args: &[String]) -> u8 {
    let id = match copy_id(args.first(), "condition") {
        Ok(i) => i,
        Err(c) => return c,
    };
    let Some(cond) = args.get(1).and_then(|c| Condition::parse(c)) else {
        eprintln!("card-ledger condition: give one of unchecked, NM, LP, MP, HP, DMG");
        return 2;
    };
    let db = match open_db() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger condition: {e}");
            return 1;
        }
    };
    match db.set_condition(id, cond) {
        Ok(true) => {
            println!("Copy {id} is now {}.", cond.code());
            0
        }
        Ok(false) => {
            eprintln!("card-ledger condition: no card with copy id {id}");
            1
        }
        Err(e) => {
            eprintln!("card-ledger condition: {e}");
            1
        }
    }
}

fn cmd_pulled(args: &[String]) -> u8 {
    let id = match copy_id(args.first(), "pulled") {
        Ok(i) => i,
        Err(c) => return c,
    };
    let pulled = match args.get(1).map(|s| s.to_ascii_lowercase()) {
        None => true,
        Some(s) if s == "yes" || s == "true" => true,
        Some(s) if s == "no" || s == "false" => false,
        Some(s) => {
            eprintln!("card-ledger pulled: {s:?} should be yes or no");
            return 2;
        }
    };
    let db = match open_db() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger pulled: {e}");
            return 1;
        }
    };
    match db.set_pulled(id, pulled) {
        Ok(true) => {
            println!(
                "Copy {id} {}.",
                if pulled {
                    "ticked as pulled"
                } else {
                    "no longer ticked"
                }
            );
            0
        }
        Ok(false) => {
            eprintln!("card-ledger pulled: no card with copy id {id}");
            1
        }
        Err(e) => {
            eprintln!("card-ledger pulled: {e}");
            1
        }
    }
}

fn cmd_carry_over(args: &[String]) -> u8 {
    let (Some(from), Some(to)) = (
        args.first().and_then(|a| a.parse::<i64>().ok()),
        args.get(1).and_then(|a| a.parse::<i64>().ok()),
    ) else {
        eprintln!("card-ledger carry-over: give the old and new item ids that `card-ledger import` printed");
        return 2;
    };
    let mut db = match open_db() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger carry-over: {e}");
            return 1;
        }
    };
    match db.carry_over(from, to) {
        Ok(n) => {
            println!("Carried over {n} card(s) from item {from} to item {to}.");
            0
        }
        Err(e) => {
            eprintln!("card-ledger carry-over: {e}");
            1
        }
    }
}

fn cmd_ui(args: &[String]) -> u8 {
    let dir = match data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger ui: {e}");
            return 1;
        }
    };
    let hook: card_ledger_ui::RefreshHook =
        Arc::new(|d: &std::path::Path| update::run(d, 24, false, false, true));
    let key_dir = dir.clone();
    let key_hook: card_ledger_ui::KeyStatusHook = Arc::new(move || {
        let store = keystore::default_store(&key_dir);
        window_key_status(justtcg::env_key().as_deref(), store.as_ref())
    });
    let code = card_ledger_ui::main_with(&dir, args, Some(hook), Some(key_hook));
    u8::try_from(code).unwrap_or(1)
}

/// The Prices tile's view of the key: on/off and its last four characters, never the key.
fn window_key_status(
    env: Option<&str>,
    store: &dyn keystore::KeyStore,
) -> card_ledger_ui::ConditionKey {
    match justtcg::key_status(env, store) {
        Ok(s) => card_ledger_ui::ConditionKey {
            on: s.is_set(),
            ends: s.ends,
            problem: None,
        },
        Err(_) => card_ledger_ui::ConditionKey {
            on: false,
            ends: None,
            problem: Some("can't read the saved key".into()),
        },
    }
}

fn cmd_justtcg_key(args: &[String]) -> u8 {
    let dir = match data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("card-ledger justtcg-key: {e}");
            return 1;
        }
    };
    let store = keystore::default_store(&dir);
    let env = justtcg::env_key();
    let mut read = || {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            eprint!("Paste your JustTCG key and press Enter (it isn't shown): ");
            let _ = std::io::stderr().flush();
        }
        keystore::read_key_from_stdin()
    };
    justtcg_key(
        args,
        env.as_deref(),
        store.as_ref(),
        &mut read,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// `justtcg-key set|status|clear`, with the env value, store, input and output given so tests
/// never touch the real Credential Manager. Nothing written to `out` or `err` contains the key.
fn justtcg_key(
    args: &[String],
    env: Option<&str>,
    store: &dyn keystore::KeyStore,
    read_key: &mut dyn FnMut() -> Result<String, String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let env_set = env.is_some_and(|e| !e.trim().is_empty());
    let env_note = |out: &mut dyn Write, still: &str| {
        if env_set {
            let _ = writeln!(
                out,
                "{} is set in your environment and is used instead of the saved key{still}.",
                justtcg::ENV_KEY
            );
        }
    };
    match (args.first().map(String::as_str), args.len()) {
        (Some("set"), 1) => {
            let key = match read_key() {
                Ok(k) => k,
                Err(e) => {
                    let _ = writeln!(err, "card-ledger justtcg-key set: {e}");
                    return 1;
                }
            };
            if let Err(why) = keystore::check_format(&key) {
                let _ = writeln!(err, "card-ledger justtcg-key set: not saved: {why}.");
                return 1;
            }
            if let Err(e) = store.set(&key) {
                let _ = writeln!(err, "card-ledger justtcg-key set: not saved: {e}");
                return 1;
            }
            let ends = keystore::mask(&key)
                .map(|e| format!(" (ends …{e})"))
                .unwrap_or_default();
            let _ = writeln!(out, "Saved your JustTCG key{ends} in {}.", store.location());
            env_note(out, "");
            0
        }
        (Some("status"), 1) => match justtcg::key_status(env, store) {
            Ok(s) => {
                let _ = writeln!(out, "JustTCG key: {}.", s.summary());
                match &s.source {
                    None => {
                        let _ = writeln!(out, "Condition prices are off. Save your key with `card-ledger justtcg-key set`; it is kept in {}.", s.store);
                    }
                    Some(KeySource::Env) => {
                        let _ = writeln!(out, "From {} (environment).", justtcg::ENV_KEY);
                        if s.stored_too {
                            let _ = writeln!(
                                out,
                                "A saved key in {} is not used while it is set.",
                                s.store
                            );
                        }
                    }
                    Some(KeySource::Stored(at)) => {
                        let _ = writeln!(out, "Stored in {at}.");
                    }
                }
                let _ = writeln!(
                    out,
                    "JustTCG's free plan is for personal, non-commercial use."
                );
                0
            }
            Err(e) => {
                let _ = writeln!(err, "card-ledger justtcg-key status: {e}");
                1
            }
        },
        (Some("clear"), 1) => match store.clear() {
            Ok(removed) => {
                if removed {
                    let _ = writeln!(
                        out,
                        "Removed the saved JustTCG key from {}.",
                        store.location()
                    );
                } else {
                    let _ = writeln!(out, "No saved JustTCG key in {}.", store.location());
                }
                env_note(out, ", so condition prices stay on until you remove it");
                0
            }
            Err(e) => {
                let _ = writeln!(err, "card-ledger justtcg-key clear: {e}");
                1
            }
        },
        _ => {
            let _ = writeln!(err, "card-ledger justtcg-key: use `set` (reads the key from standard input), `status` or `clear`.");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keystore::{KeyStore, MemoryStore};

    const KEY: &str = "test-key-0000abcd";
    const ENV: &str = "test-env-key-9999wxyz";

    /// Runs `justtcg-key <args>` with `input` as the pasted line; returns (code, out, err).
    fn run(
        args: &[&str],
        env: Option<&str>,
        store: &MemoryStore,
        input: &str,
    ) -> (u8, String, String) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut cursor = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut read = || keystore::read_key_line(&mut cursor);
        let code = justtcg_key(&args, env, store, &mut read, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn no_key_in(text: &str) {
        for k in [KEY, ENV] {
            assert!(!text.contains(k), "the key leaked: {text}");
        }
    }

    #[test]
    fn set_saves_the_trimmed_line_and_never_prints_it() {
        let s = MemoryStore::new();
        let (code, out, err) = run(
            &["set"],
            None,
            &s,
            &format!("  {KEY}  \r\nignored second line\n"),
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(s.get().unwrap().as_deref(), Some(KEY));
        assert!(out.contains("(ends …abcd)"), "{out}");
        no_key_in(&out);
        no_key_in(&err);
    }

    #[test]
    fn set_refuses_a_bad_format_without_repeating_it() {
        for bad in [
            "",
            "short",
            "has a space in the middle 1234",
            "tab\tinside-the-key-12345",
        ] {
            let s = MemoryStore::new();
            let (code, out, err) = run(&["set"], None, &s, &format!("{bad}\n"));
            assert_eq!(code, 1, "{bad:?}");
            assert_eq!(s.get().unwrap(), None);
            assert!(err.contains("not saved"), "{err}");
            if !bad.is_empty() {
                assert!(
                    !out.contains(bad) && !err.contains(bad),
                    "{bad:?} was printed: {out}{err}"
                );
            }
        }
    }

    #[test]
    fn status_shows_only_the_last_four() {
        let s = MemoryStore::new();
        let (_, out, _) = run(&["status"], None, &s, "");
        assert!(out.starts_with("JustTCG key: not set."), "{out}");
        assert!(out.contains("memory (test)"), "{out}");

        let s = MemoryStore::holding(KEY);
        let (code, out, err) = run(&["status"], None, &s, "");
        assert_eq!(code, 0);
        assert!(out.starts_with("JustTCG key: set (ends …abcd)."), "{out}");
        assert!(out.contains("Stored in memory (test)."), "{out}");
        assert!(out.contains("personal, non-commercial"), "{out}");
        no_key_in(&out);
        no_key_in(&err);
    }

    #[test]
    fn env_wins_over_the_stored_key() {
        let s = MemoryStore::holding(KEY);
        let (_, out, _) = run(&["status"], Some(ENV), &s, "");
        assert!(out.starts_with("JustTCG key: set (ends …wxyz)."), "{out}");
        assert!(
            out.contains(justtcg::ENV_KEY) && out.contains("is not used while it is set"),
            "{out}"
        );
        no_key_in(&out);
        // A blank env var counts as unset.
        let (_, out, _) = run(&["status"], Some("  "), &s, "");
        assert!(out.starts_with("JustTCG key: set (ends …abcd)."), "{out}");
    }

    #[test]
    fn clear_removes_the_saved_key() {
        let s = MemoryStore::holding(KEY);
        let (code, out, _) = run(&["clear"], None, &s, "");
        assert_eq!(code, 0);
        assert!(out.contains("Removed the saved JustTCG key"), "{out}");
        assert_eq!(s.get().unwrap(), None);
        let (_, out, _) = run(&["status"], None, &s, "");
        assert!(out.starts_with("JustTCG key: not set."), "{out}");
        let (code, out, _) = run(&["clear"], None, &s, "");
        assert_eq!(code, 0);
        assert!(out.contains("No saved JustTCG key"), "{out}");
        // With the env var still set, clear says the prices stay on.
        let (_, out, _) = run(&["clear"], Some(ENV), &s, "");
        assert!(out.contains("stay on"), "{out}");
        no_key_in(&out);
    }

    #[test]
    fn unknown_subcommands_are_refused() {
        let s = MemoryStore::new();
        for args in [&[][..], &["show"][..], &["set", KEY][..]] {
            let (code, out, err) = run(args, None, &s, "");
            assert_eq!(code, 2, "{args:?}");
            assert_eq!(
                s.get().unwrap(),
                None,
                "a key on the command line is never saved"
            );
            assert!(!out.contains(KEY) && !err.contains(KEY), "{out}{err}");
        }
    }

    #[test]
    fn window_status_carries_only_the_ending() {
        let s = MemoryStore::holding(KEY);
        let k = window_key_status(None, &s);
        assert_eq!(
            k,
            card_ledger_ui::ConditionKey {
                on: true,
                ends: Some("abcd".into()),
                problem: None
            }
        );
        assert!(!format!("{k:?}").contains(KEY));
        let k = window_key_status(Some(ENV), &s);
        assert_eq!(k.ends.as_deref(), Some("wxyz"));
        assert!(!window_key_status(None, &MemoryStore::new()).on);
    }
}

//! Runs the window against `CARD_LEDGER_DATA_DIR` (or %APPDATA%\card-ledger) for manual checks:
//! `cargo run -p card-ledger-ui --example serve -- --port 8795 --no-open`.

fn main() {
    let dir = match card_ledger_core::data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("serve: {e}");
            std::process::exit(2);
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(card_ledger_ui::main(&dir, &args));
}

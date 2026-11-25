//! card-ledger core: the Collectr import, the ledger store, identity, pricing and piles.
//! No network code lives here (the `card-ledger-fetch` crate implements `card::CardLookup`).

pub mod card;
pub mod db;
pub mod identity;
pub mod import;
pub mod ledger;
pub mod model;
pub mod piles;
pub mod pricing;
pub mod time;

use std::path::PathBuf;

/// Where the ledger keeps its database: `CARD_LEDGER_DATA_DIR` if set; else `%APPDATA%\card-ledger`
/// on Windows; else `$XDG_DATA_HOME/card-ledger` or `~/.local/share/card-ledger`.
pub fn data_dir() -> Result<PathBuf, String> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    };
    if let Some(d) = var("CARD_LEDGER_DATA_DIR") {
        return Ok(d);
    }
    if cfg!(windows) {
        return var("APPDATA")
            .map(|d| d.join("card-ledger"))
            .ok_or_else(|| "neither CARD_LEDGER_DATA_DIR nor APPDATA is set".to_string());
    }
    var("XDG_DATA_HOME")
        .or_else(|| var("HOME").map(|h| h.join(".local").join("share")))
        .map(|d| d.join("card-ledger"))
        .ok_or_else(|| "set CARD_LEDGER_DATA_DIR (no XDG_DATA_HOME or HOME either)".to_string())
}

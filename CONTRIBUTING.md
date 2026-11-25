# Contributing

Thanks for helping. A few ground rules keep this project useful and on the right side of other people's rights.

## Before you open a pull request

```
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same commands on Windows and Linux. Tests never touch the network; the one live test is `#[ignore]`.

## Rules for code and data

- **Never default a card to Near Mint.** Unchecked stays unchecked until the owner sets a condition.
- **Every price shows its source and date.** A price is never borrowed from another printing or currency.
- **No shipped card data, images or prices.** Card data, images and prices are fetched at runtime. Test fixtures are made up or minimal: real card names and ids are fine as facts, but prices must be invented example values, and there's no card text.
- **No scraping and no getting around bot checks.** Only public APIs, one request at a time, cached, honouring 429 and Retry-After.
- **No personal data.** Never commit a real collection export, a ledger database, an API key or a home-folder path.
- **Keys are never printed or logged**, and they're sent only to their own service.

## Reporting a card that matched wrong

Use the "Card mismatch" issue template. Give the set name, card number and printing (holo, reverse holo, 1st Edition…) as your export shows them, and what the card actually is. **Please don't attach your export file.**

## What this changes

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass
- [ ] No real collection exports, ledger databases, API keys or home-folder paths
- [ ] No shipped card data, images or real prices (test fixtures use made-up values)
- [ ] Prices still show their source and date; nothing defaults to Near Mint

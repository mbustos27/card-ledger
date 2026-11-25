# Changelog

All notable changes. Dates are UTC.

## 0.1.1 (2026-11-25)

### Changed
- **Extra copies go to the box.** One copy of each card is the keeper and takes the pile its value earns. The keeper is the best condition among checked copies, or copy 1 until any is checked. Every other copy goes to the box as "Extra copy → box", and its value still counts in the totals. Graded lines are their own card. An unchecked extra is flagged "check condition" when it might be worth more than a checked keeper.
- `summary` notes when a build carries no research values.

### Added
- `card-ledger match`: match cards to TCGdex without fetching prices.
- `card-ledger ui --no-images`: placeholders instead of card art.
- A banner (light and dark), badges, a "Why card-ledger" list and a how-it-works diagram in the README.
- README quick start, FAQ and screenshots (sample data); CI (format, clippy, tests on Windows and Linux); a release workflow that builds the Windows zip and checksums from a tag; issue and PR templates.

## 0.1.0 (2026-11-25)

First release.

- Import a Collectr CSV export. Every card starts unchecked; re-imports keep conditions and never delete.
- Card identity via TCGdex, with scanner-mistake flags.
- Prices by condition: TCGplayer NM via TCGdex, optional JustTCG per-condition prices (your own key, stored in Windows Credential Manager), research ranges, and condition multipliers.
- Piles: toploader, small binder, big binder (Pokédex order), box, set aside.
- A local window on 127.0.0.1:8785, readable at phone width.

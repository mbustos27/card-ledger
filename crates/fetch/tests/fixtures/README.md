# Test fixtures

These files are made-up, minimal samples shaped like TCGdex API answers. They are not
real data and were not recorded from the live API.

- Only the fields the parser reads are kept: `id`, `localId`, `name`, `set {id, name}`,
  `category`, `rarity`, `dexId`, `variants`, `image`, and `pricing.tcgplayer` /
  `pricing.cardmarket` with their bucket keys and `updated` timestamps.
- Every price is an invented round example value (10.00, 2.50, 0.25 and so on), not a
  market figure. Product ids are invented, except the TCGplayer id on xy1-121. The
  TCGplayer ids on dc1-6 and xy1-121 are the join keys `justtcg_batch_from_docs.json` uses.
- Card names, card ids and set ids are used as keys only, and the cards were picked for the
  shapes they test (an EX priced only as holofoil, a Radiant Collection number, a promo, a
  1st Edition WotC card, a Japanese card, a 404). No card text, artwork or image files are
  included; image URLs only show the `https://assets.tcgdex.net/...` form.
- The TCGplayer holofoil bucket on `en_cards_xyp-XY150.json` is a stand-in: TCGdex has no
  TCGplayer data for that card.
- `en_sets_dc1.json` is cut down to three cards.

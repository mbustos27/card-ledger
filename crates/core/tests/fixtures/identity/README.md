# Identity test fixtures

Made-up, minimal samples shaped like TCGdex v2 API answers (`/v2/{lang}/cards/{id}` and
`/v2/{lang}/sets/{id}`). They are not copies of TCGdex data.

- Each card keeps only the fields the test's fake lookup reads: `id`, `localId`, `name`,
  `category`, `rarity`, `set` (`id`, `name`), `dexId`, `variants`, and the TCGplayer price
  bucket names under `pricing.tcgplayer`.
- Bucket names matter (a printing counts as existing when `variants` lists it or a TCGplayer
  bucket for it exists). The prices inside them are placeholder round numbers, not real prices.
- The cards were picked for the edge case each one exercises: an EX TCGdex lists as normal
  only but TCGplayer prices as holofoil (dc1-6), Radiant Collection numbers (g1-RC7,
  bw11-RC13), a three-digit black-star promo (xyp-XY150), a reverse holo only TCGplayer knows
  about (ex14-44), a holo-and-reverse rare (bw7-41), a normal-and-reverse common (bw11-37), a
  rare whose holo and reverse show only as TCGplayer buckets (xy1-28), a 1st Edition WotC card
  (base1-24), the two decks of a trainer kit (tk-xy-sy-14, tk-xy-n-14) and a Japanese card
  (SV2a-007).
- The holofoil bucket on xyp-XY150 is a stand-in: TCGdex has no TCGplayer data for that card.
  It keeps the "EX promo priced as holofoil" path under test.
- No card text, artwork links, illustrators, attacks, abilities, effects, descriptions or
  legality data.
- The set listing (`en_set_dc1.json`) holds only the few entries the name-fallback test needs.
- File names are `{lang}_{card id}.json` for cards and `{lang}_set_{set id}.json` for listings.

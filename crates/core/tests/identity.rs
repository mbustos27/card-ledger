//! Card identity: set map, number normalising, matching against recorded TCGdex answers, flags.
//! Fixtures under tests/fixtures/identity are made-up minimal samples shaped like TCGdex v2
//! answers (see the README there): identity fields, variants and TCGplayer bucket names only.

use card_ledger_core::card::{
    CardBrief, CardLookup, CardRecord, FlagCode, Lang, LookupError, MarketPrice, Printings,
    Resolution,
};
use card_ledger_core::db::Item;
use card_ledger_core::identity::{
    is_pokemon_catalog_category, is_pokemon_category, looks_japanese, names_match, normalize_name,
    normalize_number, price_flags, resolve, tcgdex_local_id, SetMap,
};
use card_ledger_core::model::Variant;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

// ---- recorded TCGdex answers --------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/identity")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcgCard {
    id: String,
    local_id: String,
    name: String,
    rarity: Option<String>,
    category: Option<String>,
    set: TcgSetRef,
    #[serde(default)]
    variants: Option<TcgVariants>,
    #[serde(default)]
    dex_id: Vec<u32>,
    pricing: Option<TcgPricing>,
}

#[derive(Deserialize)]
struct TcgSetRef {
    id: String,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TcgVariants {
    #[serde(default)]
    normal: bool,
    #[serde(default)]
    reverse: bool,
    #[serde(default)]
    holo: bool,
    #[serde(default)]
    first_edition: bool,
}

#[derive(Deserialize)]
struct TcgPricing {
    tcgplayer: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcgBucket {
    product_id: Option<u64>,
    low_price: Option<f64>,
    market_price: Option<f64>,
}

#[derive(Deserialize)]
struct TcgSet {
    cards: Vec<TcgBrief>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcgBrief {
    id: String,
    local_id: String,
    name: String,
}

fn bucket_variant(bucket: &str) -> Option<Variant> {
    match bucket {
        "normal" => Some(Variant::Normal),
        "holofoil" => Some(Variant::Holofoil),
        "reverse-holofoil" => Some(Variant::ReverseHolofoil),
        "1st-edition-holofoil" => Some(Variant::FirstEditionHolofoil),
        "1st-edition" => Some(Variant::FirstEdition),
        "unlimited-holofoil" => Some(Variant::UnlimitedHolofoil),
        _ => None,
    }
}

fn record_from_json(lang: Lang, text: &str) -> CardRecord {
    let c: TcgCard = serde_json::from_str(text).expect("fixture card");
    let v = c.variants.unwrap_or_default();
    let mut prices = Vec::new();
    if let Some(tcg) = c.pricing.and_then(|p| p.tcgplayer) {
        let updated = tcg
            .get("updated")
            .and_then(|u| u.as_str())
            .map(String::from);
        for (bucket, val) in &tcg {
            if !val.is_object() {
                continue;
            }
            let b: TcgBucket = serde_json::from_value(val.clone()).expect("bucket");
            prices.push(MarketPrice {
                source: "tcgplayer".into(),
                bucket: bucket.clone(),
                variant: bucket_variant(bucket),
                market: b.market_price,
                low: b.low_price,
                currency: "USD".into(),
                updated: updated.clone(),
                product_id: b.product_id,
            });
        }
    }
    CardRecord {
        lang,
        id: c.id,
        set_id: c.set.id,
        set_name: c.set.name,
        local_id: c.local_id,
        name: c.name,
        rarity: c.rarity,
        category: c.category,
        dex_ids: c.dex_id,
        printings: Printings {
            normal: v.normal,
            reverse: v.reverse,
            holo: v.holo,
            first_edition: v.first_edition,
        },
        prices,
        image: None,
    }
}

/// A `CardLookup` backed by the recorded fixtures.
#[derive(Default)]
struct Fake {
    cards: HashMap<(Lang, String), CardRecord>,
    sets: HashMap<(Lang, String), Vec<CardBrief>>,
    fail: Option<LookupError>,
    fail_sets: Option<LookupError>,
    calls: Vec<String>,
}

impl Fake {
    fn recorded() -> Fake {
        let mut f = Fake::default();
        for entry in std::fs::read_dir(fixture_dir()).expect("fixture dir") {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            if !file.ends_with(".json") {
                continue; // README.md
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let (lang, rest) = file.split_once('_').unwrap();
            let lang = Lang::parse(lang).unwrap();
            let rest = rest.trim_end_matches(".json");
            if let Some(set_id) = rest.strip_prefix("set_") {
                let s: TcgSet = serde_json::from_str(&text).expect("fixture set");
                let briefs = s
                    .cards
                    .into_iter()
                    .map(|b| CardBrief {
                        id: b.id,
                        local_id: b.local_id,
                        name: b.name,
                    })
                    .collect();
                f.sets.insert((lang, set_id.to_string()), briefs);
            } else {
                let rec = record_from_json(lang, &text);
                assert_eq!(rec.id, rest, "fixture file name matches its id");
                f.cards.insert((lang, rec.id.clone()), rec);
            }
        }
        f
    }

    fn card_mut(&mut self, id: &str) -> &mut CardRecord {
        self.cards.get_mut(&(Lang::En, id.to_string())).unwrap()
    }
}

impl CardLookup for Fake {
    fn card(&mut self, lang: Lang, id: &str) -> Result<Option<CardRecord>, LookupError> {
        self.calls.push(format!("card {}:{id}", lang.code()));
        if let Some(e) = &self.fail {
            return Err(e.clone());
        }
        Ok(self.cards.get(&(lang, id.to_string())).cloned())
    }
    fn set_cards(
        &mut self,
        lang: Lang,
        set_id: &str,
    ) -> Result<Option<Vec<CardBrief>>, LookupError> {
        self.calls.push(format!("set {}:{set_id}", lang.code()));
        if let Some(e) = &self.fail_sets {
            return Err(e.clone());
        }
        Ok(self.sets.get(&(lang, set_id.to_string())).cloned())
    }
}

fn item(set: &str, name: &str, number: &str, variant: Variant) -> Item {
    Item {
        id: 7,
        portfolio: "Main".into(),
        category: "Pokemon".into(),
        set_name: set.into(),
        product_name: name.into(),
        card_number: number.into(),
        rarity: String::new(),
        variant,
        grade: "Ungraded".into(),
        collectr_condition: "Near Mint".into(),
        collectr_price: None,
        collectr_price_date: None,
        export_quantity: 1,
        in_last_export: true,
    }
}

fn run(it: &Item) -> Resolution {
    resolve(it, &SetMap::embedded(), &mut Fake::recorded()).expect("lookup ok")
}

fn codes(r: &Resolution) -> Vec<FlagCode> {
    r.flags.iter().map(|f| f.code).collect()
}

fn has(r: &Resolution, code: FlagCode) -> bool {
    r.flags.iter().any(|f| f.code == code)
}

// ---- numbers and names --------------------------------------------------------------------

#[test]
fn numbers_normalise_to_tcgdex_form() {
    assert_eq!(normalize_number("040/087"), "40");
    assert_eq!(normalize_number("064"), "64");
    assert_eq!(normalize_number("RC9"), "RC9");
    assert_eq!(normalize_number("rc09"), "RC9");
    assert_eq!(normalize_number("XY150"), "XY150");
    assert_eq!(normalize_number("XY01"), "XY1");
    assert_eq!(normalize_number("LOB-062"), "LOB-062");
    assert_eq!(normalize_number(" 151/172 "), "151");
    assert_eq!(normalize_number("210/198"), "210");
    assert_eq!(normalize_number("000"), "0");
    assert_eq!(normalize_number("28a"), "28a");
    assert_eq!(normalize_number("#7"), "7");
    assert_eq!(normalize_number(""), "");
    // TCGdex's padded forms, per set.
    assert_eq!(tcgdex_local_id("40", 3), "040");
    assert_eq!(tcgdex_local_id("151", 3), "151");
    assert_eq!(tcgdex_local_id("6", 0), "6");
    assert_eq!(tcgdex_local_id("XY1", 2), "XY01");
    assert_eq!(tcgdex_local_id("XY150", 2), "XY150");
    assert_eq!(tcgdex_local_id("HGSS18", 2), "HGSS18");
}

#[test]
fn names_fold_collectr_suffixes() {
    assert!(names_match("Zubat (47)", "Zubat"));
    assert!(names_match("Togepi (JP)", "Togepi"));
    assert!(names_match("Mightyena (Omega)", "Mightyena"));
    assert!(names_match("Vivillon (Meadow [Pink])", "Vivillon"));
    assert!(names_match("Blastoise EX (142 Full Art)", "Blastoise EX"));
    assert!(names_match("Black Kyurem EX", "Black Kyurem-EX"));
    assert!(names_match("Iron Hands ex", "Iron Hands EX"));
    assert!(names_match("Deoxys EX (Team Plasma)", "Deoxys-EX"));
    assert!(names_match("Sneasel (Team Plasma) (64)", "Sneasel"));
    assert!(names_match("Flabebe (103)", "Flabébé"));
    assert!(names_match("Nidoran M", "Nidoran♂"));
    assert!(names_match("Mudkip (Delta Species)", "Mudkip δ"));
    assert!(names_match("Unown [Q]", "Unown Q"));
    assert!(names_match("Palkia Lv.X", "Palkia"));
    assert!(names_match(
        "Team Aqua's Kyogre EX (Full Art)",
        "Team Aqua’s Kyogre-EX"
    ));
    assert!(names_match("Castform Rain", "Castform Rain Form"));
    // The example in `names_match`'s doc comment.
    assert!(names_match("Shellos East", "Shellos East Sea Form"));
    assert!(names_match("Iron Crown (Poke Ball Pattern)", "Iron Crown"));
    // Real differences stay different.
    assert!(!names_match("Gengar", "Gengar EX"));
    assert!(!names_match("Gengar EX", "M Gengar EX"));
    assert!(!names_match("Pidgey", "Spritzee"));
    assert!(!names_match("", ""));
    assert_eq!(
        normalize_name("Team Aqua's Kyogre-EX"),
        "team aquas kyogre ex"
    );
}

// ---- the set map --------------------------------------------------------------------------

#[test]
fn set_map_examples() {
    let m = SetMap::embedded();
    let id = |n: &str| m.get(n).and_then(|e| e.set_id()).map(String::from);
    assert_eq!(id("Double Crisis").as_deref(), Some("dc1"));
    assert_eq!(id("Base Set (Unlimited)").as_deref(), Some("base1"));
    assert_eq!(id("XY Promos").as_deref(), Some("xyp"));
    assert_eq!(m.get("XY Promos").unwrap().pad, 2);
    assert_eq!(m.get("Silver Tempest").unwrap().pad, 3);

    let lt = m.get("Legendary Treasures: Radiant Collections").unwrap();
    assert_eq!(lt.set_id(), Some("bw11"));
    assert!(lt.radiant);
    assert_eq!(lt.rc_max, Some(25));
    let gen = m.get("Generations: Radiant Collection").unwrap();
    assert_eq!(gen.set_id(), Some("g1"));
    assert_eq!(gen.rc_max, Some(32));

    let kit = m.get("XY Trainer Kit: Bisharp & Wigglytuff").unwrap();
    assert_eq!(
        kit.set_ids,
        vec!["tk-xy-b".to_string(), "tk-xy-w".to_string()]
    );

    let ygo = m.get("The Legend of Blue Eyes White Dragon").unwrap();
    assert!(ygo.not_pokemon);
    assert_eq!(ygo.set_id(), None);

    let anniv = m.get("20th Anniversary").unwrap();
    assert_eq!(anniv.lang, Lang::Ja);
    assert!(anniv.none.as_deref().unwrap().contains("CP6"));
    assert!(m.get("Megalo Cannon").unwrap().none.is_some());
    assert!(m.get("Gym Challenge (Japanese)").unwrap().none.is_some());
    assert!(m.get("No Such Set").is_none());
}

#[test]
fn set_map_rejects_bad_files() {
    assert!(SetMap::parse("[[set]]\nname = \"A\"\nid = \"a1\"\n").is_ok());
    let dup = "[[set]]\nname = \"A\"\nid = \"a1\"\n[[set]]\nname = \"A\"\nid = \"a2\"\n";
    assert!(SetMap::parse(dup).unwrap_err().contains("twice"));
    let both = "[[set]]\nname = \"A\"\nid = \"a1\"\nnone = \"why\"\n";
    assert!(SetMap::parse(both).is_err());
    let neither = "[[set]]\nname = \"A\"\n";
    assert!(SetMap::parse(neither).is_err());
    let lang = "[[set]]\nname = \"A\"\nid = \"a1\"\nlang = \"fr\"\n";
    assert!(SetMap::parse(lang).unwrap_err().contains("lang"));
    let typo = "[[set]]\nname = \"A\"\nid = \"a1\"\nrcmax = 3\n";
    assert!(SetMap::parse(typo).is_err());
    let bad_id = "[[set]]\nname = \"A\"\nid = \"../x\"\n";
    assert!(SetMap::parse(bad_id).is_err());
}

// ---- matching -----------------------------------------------------------------------------

#[test]
fn exact_id_hits() {
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    ));
    let c = r.card.as_ref().expect("resolved");
    assert_eq!(c.card_id, "dc1-6");
    assert_eq!(c.canonical, "dc1-6/holo");
    assert_eq!(c.dex_id, Some(382));
    assert_eq!(c.rarity.as_deref(), Some("Ultra Rare"));
    assert_eq!(c.category.as_deref(), Some("Pokemon"));
    assert_eq!(r.reason, None);
    // TCGdex calls this card "normal" only, but TCGplayer prices a holofoil: no variant flag.
    assert!(r.flags.is_empty(), "{:?}", r.flags);

    let r = run(&item(
        "Generations: Radiant Collection",
        "Snorunt",
        "RC7",
        Variant::Holofoil,
    ));
    assert_eq!(r.card.as_ref().unwrap().canonical, "g1-RC7/holo");
    assert!(r.flags.is_empty(), "{:?}", r.flags);

    let r = run(&item(
        "Legendary Treasures: Radiant Collections",
        "Purrloin",
        "RC13",
        Variant::Holofoil,
    ));
    assert_eq!(r.card.as_ref().unwrap().canonical, "bw11-RC13/holo");
    assert!(r.flags.is_empty(), "{:?}", r.flags);

    let r = run(&item("XY Promos", "Yveltal EX", "XY150", Variant::Holofoil));
    assert_eq!(r.card.as_ref().unwrap().canonical, "xyp-XY150/holo");
    assert_eq!(codes(&r), vec![FlagCode::PromoVsSet]);

    let r = run(&item(
        "XY Base Set",
        "Talonflame (28)",
        "28",
        Variant::ReverseHolofoil,
    ));
    assert_eq!(r.card.as_ref().unwrap().canonical, "xy1-28/reverse");
    assert!(r.flags.is_empty(), "{:?}", r.flags);
}

#[test]
fn name_fallback_from_the_set_listing() {
    // Wrong number: dc1-99 doesn't exist, so the listing is searched by name.
    let mut fake = Fake::recorded();
    let it = item(
        "Double Crisis",
        "Team Aqua's Kyogre-EX (Full Art)",
        "99",
        Variant::Holofoil,
    );
    let r = resolve(&it, &SetMap::embedded(), &mut fake).unwrap();
    assert_eq!(r.card.as_ref().unwrap().card_id, "dc1-6");
    assert!(!has(&r, FlagCode::NameMismatch));
    assert_eq!(
        fake.calls,
        vec!["card en:dc1-99", "set en:dc1", "card en:dc1-6"]
    );
    // The number was silently different: say so, never fix it quietly.
    assert_eq!(codes(&r), vec![FlagCode::NumberDiffers], "{:?}", r.flags);
    let m = &r.flags[0].message;
    assert!(m.contains("#99") && m.contains("#6"), "{m}");

    // A padding mismatch is caught by number in the listing ("006" isn't an id, "6" is).
    let map = SetMap::parse("[[set]]\nname = \"DC\"\nid = \"dc1\"\npad = 3\n").unwrap();
    let mut fake = Fake::recorded();
    let r = resolve(
        &item("DC", "Team Aqua's Kyogre EX", "006/034", Variant::Holofoil),
        &map,
        &mut fake,
    )
    .unwrap();
    assert_eq!(r.card.as_ref().unwrap().card_id, "dc1-6");
    assert_eq!(fake.calls[0], "card en:dc1-006");
    assert!(
        !has(&r, FlagCode::NumberDiffers),
        "same number, other padding: {:?}",
        r.flags
    );

    // Found by name with no number at all: nothing to disagree with.
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "",
        Variant::Holofoil,
    ));
    assert_eq!(r.card.as_ref().unwrap().card_id, "dc1-6");
    assert!(!has(&r, FlagCode::NumberDiffers), "{:?}", r.flags);

    // Neither number nor name: unresolved with a reason, nothing guessed.
    let r = run(&item("Double Crisis", "Kyogre", "99", Variant::Normal));
    assert!(r.card.is_none());
    assert!(
        r.reason.as_deref().unwrap().contains("no card numbered 99"),
        "{:?}",
        r.reason
    );

    // A set TCGdex doesn't have at all.
    let map = SetMap::parse("[[set]]\nname = \"Gone\"\nid = \"zz9\"\n").unwrap();
    let r = resolve(
        &item("Gone", "Pikachu", "1", Variant::Normal),
        &map,
        &mut Fake::recorded(),
    )
    .unwrap();
    assert_eq!(r.reason.as_deref(), Some("TCGdex has no set zz9"));
}

#[test]
fn trainer_kit_picks_the_deck_whose_name_matches() {
    // Collectr drops the accent; TCGdex writes "Pokémon Catcher".
    let r = run(&item(
        "XY Trainer Kit: Sylveon & Noivern",
        "Pokemon Catcher (#14)",
        "14",
        Variant::Normal,
    ));
    let c = r.card.as_ref().expect("resolved");
    assert_eq!(c.card_id, "tk-xy-n-14", "tk-xy-sy-14 is a Granbull");
    assert!(!has(&r, FlagCode::NameMismatch), "{:?}", r.flags);
}

#[test]
fn unknown_set_is_unresolved_with_reason() {
    let r = run(&item("Some New Set", "Pikachu", "1", Variant::Normal));
    assert!(r.card.is_none());
    assert!(r.reason.unwrap().contains("isn't in data/sets.toml"));
}

#[test]
fn sets_tcgdex_lacks_are_unresolved_with_reason() {
    let r = run(&item(
        "20th Anniversary",
        "Togepi (JP)",
        "040/087",
        Variant::Holofoil,
    ));
    assert!(r.card.is_none());
    assert!(r.reason.as_deref().unwrap().contains("CP6"));
    assert!(has(&r, FlagCode::Japanese));

    let r = run(&item(
        "SV: ex Starter Set Quaxly & Mimikyu ex",
        "Quaxly",
        "009/021",
        Variant::Normal,
    ));
    assert!(r.card.is_none());
    assert!(r.reason.is_some());
}

#[test]
fn yugioh_line_is_unresolved_as_not_pokemon() {
    let mut it = item(
        "The Legend of Blue Eyes White Dragon",
        "Mystical Elf",
        "LOB-062",
        Variant::FirstEdition,
    );
    it.category = "YuGiOh".into();
    let mut fake = Fake::recorded();
    let r = resolve(&it, &SetMap::embedded(), &mut fake).unwrap();
    assert!(r.card.is_none());
    assert_eq!(r.reason.as_deref(), Some("not a Pokémon card"));
    assert_eq!(codes(&r), vec![FlagCode::NotPokemon]);
    assert!(fake.calls.is_empty(), "no lookups for a non-Pokémon card");

    // The category alone is enough, whatever the set says.
    let mut it = item("Double Crisis", "Mystical Elf", "6", Variant::Normal);
    it.category = "YuGiOh".into();
    assert_eq!(run(&it).reason.as_deref(), Some("not a Pokémon card"));
}

#[test]
fn lookup_errors_pass_through() {
    let it = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    );
    let mut fake = Fake::recorded();
    fake.fail = Some(LookupError::Unavailable("timeout".into()));
    let e = resolve(&it, &SetMap::embedded(), &mut fake).unwrap_err();
    assert_eq!(e, LookupError::Unavailable("timeout".into()));

    // An error on the listing (after the id missed) is also "try later", not "unresolved".
    let mut fake = Fake::recorded();
    fake.fail_sets = Some(LookupError::RateLimited("429".into()));
    let it = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "99",
        Variant::Holofoil,
    );
    let e = resolve(&it, &SetMap::embedded(), &mut fake).unwrap_err();
    assert_eq!(e, LookupError::RateLimited("429".into()));
}

#[test]
fn japanese_cards_use_the_ja_catalogue() {
    let map = SetMap::parse(
        "[[set]]\nname = \"Pokemon Card 151\"\nid = \"SV2a\"\nlang = \"ja\"\npad = 3\n",
    )
    .unwrap();
    let mut fake = Fake::recorded();
    let r = resolve(
        &item(
            "Pokemon Card 151",
            "Squirtle (JP)",
            "007/165",
            Variant::Normal,
        ),
        &map,
        &mut fake,
    )
    .unwrap();
    let c = r.card.as_ref().expect("resolved");
    assert_eq!(c.canonical, "ja:SV2a-007/normal");
    assert_eq!(c.lang, Lang::Ja);
    assert_eq!(c.dex_id, Some(7));
    assert_eq!(fake.calls, vec!["card ja:SV2a-007"]);
    // Japanese names can't be compared with Collectr's English ones.
    assert_eq!(codes(&r), vec![FlagCode::Japanese]);

    // "(JP)" in the name flags an English-mapped line too. It still resolves to the English card,
    // so the flag is what tells the ledger not to use the English USD price.
    let jp = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX (JP)",
        "6",
        Variant::Holofoil,
    );
    assert!(looks_japanese(&jp));
    let r = run(&jp);
    assert_eq!(r.card.as_ref().unwrap().canonical, "dc1-6/holo");
    assert_eq!(r.card.as_ref().unwrap().lang, Lang::En);
    assert!(has(&r, FlagCode::Japanese), "{:?}", r.flags);
    // Lower case and no other marks: same.
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX (jp)",
        "6",
        Variant::Holofoil,
    ));
    assert!(has(&r, FlagCode::Japanese), "{:?}", r.flags);

    // Collectr's "Pokemon Japan" category marks a Japanese card even without "(JP)".
    let mut it = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    );
    it.category = "Pokemon Japan".into();
    assert!(looks_japanese(&it));
    let r = run(&it);
    assert!(r.card.is_some(), "still a Pokémon card: {:?}", r.reason);
    assert!(has(&r, FlagCode::Japanese), "{:?}", r.flags);

    // A plain English line is not Japanese.
    let en = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    );
    assert!(!looks_japanese(&en));
    assert!(!has(&run(&en), FlagCode::Japanese));
}

#[test]
fn pokemon_category_checks() {
    // Collectr's category column.
    for c in [
        "Pokemon",
        "Pokémon",
        "pokemon",
        "POKEMON",
        "Pokemon Japan",
        " Pokemon ",
        "",
    ] {
        assert!(is_pokemon_category(c), "{c:?} is Pokémon");
    }
    for c in ["YuGiOh", "Magic: The Gathering", "One Piece", "Lorcana"] {
        assert!(!is_pokemon_category(c), "{c:?} is not Pokémon");
    }
    // A blank category (older exports) resolves like a Pokémon card.
    let mut it = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    );
    it.category = String::new();
    assert_eq!(run(&it).card.unwrap().card_id, "dc1-6");

    // TCGdex's category field.
    assert!(is_pokemon_catalog_category(None));
    // A blank field is no answer, the same as None.
    assert!(is_pokemon_catalog_category(Some(" ")));
    for c in ["Pokemon", "Trainer", "Energy"] {
        assert!(is_pokemon_catalog_category(Some(c)), "{c}");
    }
    for c in ["YuGiOh", "Monster", "pokemon tcg", "Item"] {
        assert!(!is_pokemon_catalog_category(Some(c)), "{c}");
    }
    // A catalogue card in another category is unresolved as not Pokémon.
    let mut fake = Fake::recorded();
    fake.card_mut("dc1-6").category = Some("Monster".into());
    let it = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    );
    let r = resolve(&it, &SetMap::embedded(), &mut fake).unwrap();
    assert!(r.card.is_none());
    assert!(has(&r, FlagCode::NotPokemon), "{:?}", r.flags);
}

// ---- flags --------------------------------------------------------------------------------

#[test]
fn reverse_holo_on_a_card_with_no_reverse() {
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::ReverseHolofoil,
    ));
    assert_eq!(codes(&r), vec![FlagCode::VariantNotListed]);
    assert!(r.flags[0].message.contains("reverse holo"));
    assert_eq!(
        r.card.unwrap().canonical,
        "dc1-6/reverse",
        "flagged, never changed"
    );
}

#[test]
fn reverse_holo_counts_when_only_tcgplayer_prices_it() {
    // ex14-44 says reverse=false, but TCGplayer prices a reverse-holofoil.
    let r = run(&item(
        "EX Crystal Guardians",
        "Aron",
        "44",
        Variant::ReverseHolofoil,
    ));
    assert!(!has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);
}

#[test]
fn other_variant_conflicts() {
    // bw7-41 Samurott is holo + reverse only: a Normal scan is suspicious.
    let r = run(&item(
        "Boundaries Crossed",
        "Samurott",
        "41",
        Variant::Normal,
    ));
    assert!(has(&r, FlagCode::VariantNotListed));
    // bw11-37 Oshawott is normal + reverse: "Holofoil" is likely a reverse holo.
    let r = run(&item(
        "Legendary Treasures",
        "Oshawott",
        "37",
        Variant::Holofoil,
    ));
    assert!(has(&r, FlagCode::VariantNotListed));
    let r = run(&item(
        "Legendary Treasures",
        "Oshawott",
        "37",
        Variant::ReverseHolofoil,
    ));
    assert!(!has(&r, FlagCode::VariantNotListed));
    // 1st Edition on a modern card.
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::FirstEdition,
    ));
    assert!(has(&r, FlagCode::VariantNotListed));
    assert!(has(&r, FlagCode::FirstEditionCheck));
}

#[test]
fn holofoil_on_a_normal_only_card() {
    // base1-24 Charmeleon is an Uncommon: TCGdex lists normal + 1st Edition, TCGplayer prices only
    // "normal". A "Holofoil" scan is suspicious.
    let r = run(&item(
        "Base Set (Unlimited)",
        "Charmeleon",
        "24",
        Variant::Holofoil,
    ));
    assert!(has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);
    let f = r
        .flags
        .iter()
        .find(|f| f.code == FlagCode::VariantNotListed)
        .unwrap();
    assert!(
        !f.message.contains("reverse holo"),
        "no reverse holo exists: {}",
        f.message
    );
    assert_eq!(
        r.card.unwrap().canonical,
        "base1-24/holo",
        "flagged, never changed"
    );

    // The same rule on a strictly normal-only card (trainer-kit Granbull: normal variant and
    // bucket).
    let r = run(&item(
        "XY Trainer Kit: Sylveon & Noivern",
        "Granbull",
        "14",
        Variant::Holofoil,
    ));
    assert_eq!(r.card.as_ref().unwrap().card_id, "tk-xy-sy-14");
    assert!(has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);
    let r = run(&item(
        "XY Trainer Kit: Sylveon & Noivern",
        "Granbull",
        "14",
        Variant::Normal,
    ));
    assert!(!has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);

    // EX and full-art cards: TCGdex says normal only, TCGplayer prices a holofoil. No flag. (The
    // promo's holofoil bucket is a stand-in: TCGdex has no TCGplayer data for xyp-XY150.)
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    ));
    assert!(!has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);
    let r = run(&item("XY Promos", "Yveltal EX", "XY150", Variant::Holofoil));
    assert!(!has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);
    // Radiant Collection holos: TCGdex says normal only and TCGplayer prices nothing. With no
    // price evidence at all the variants object can't rule a holo out.
    let r = run(&item(
        "Generations: Radiant Collection",
        "Snorunt",
        "RC7",
        Variant::Holofoil,
    ));
    assert!(!has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);
    let r = run(&item(
        "Legendary Treasures: Radiant Collections",
        "Purrloin",
        "RC13",
        Variant::Holofoil,
    ));
    assert!(!has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);

    // A normal-only card whose holofoil only TCGplayer knows about: the bucket is the evidence.
    let mut fake = Fake::recorded();
    let rec = fake.card_mut("tk-xy-sy-14");
    let mut holo = rec.prices[0].clone();
    holo.bucket = "holofoil".into();
    holo.variant = Some(Variant::Holofoil);
    rec.prices.push(holo);
    let it = item(
        "XY Trainer Kit: Sylveon & Noivern",
        "Granbull",
        "14",
        Variant::Holofoil,
    );
    let r = resolve(&it, &SetMap::embedded(), &mut fake).unwrap();
    assert!(!has(&r, FlagCode::VariantNotListed), "{:?}", r.flags);
}

#[test]
fn unknown_printings_are_not_flagged() {
    let mut fake = Fake::recorded();
    let rec = fake.card_mut("dc1-6");
    rec.printings = Printings::default();
    rec.prices.clear();
    let it = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::ReverseHolofoil,
    );
    let r = resolve(&it, &SetMap::embedded(), &mut fake).unwrap();
    assert!(r.flags.is_empty(), "{:?}", r.flags);
    // Holofoil on a card with nothing known is unknown too.
    let it = item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    );
    let r = resolve(&it, &SetMap::embedded(), &mut fake).unwrap();
    assert!(r.flags.is_empty(), "{:?}", r.flags);
}

#[test]
fn wotc_first_edition_and_base_set_lookalikes() {
    let r = run(&item(
        "Base Set (Unlimited)",
        "Charmeleon",
        "24",
        Variant::Normal,
    ));
    assert_eq!(r.card.as_ref().unwrap().canonical, "base1-24/normal");
    assert_eq!(
        codes(&r),
        vec![FlagCode::FirstEditionCheck, FlagCode::BaseSetLookalike]
    );
    assert!(r.flags[1].message.contains("/102"));
    let r = run(&item(
        "Base Set (Unlimited)",
        "Charmeleon",
        "24",
        Variant::FirstEdition,
    ));
    assert!(
        !has(&r, FlagCode::VariantNotListed),
        "base1-24 has a 1st Edition"
    );
}

#[test]
fn radiant_collection_numbers() {
    // Out of range: bw11's Radiant Collection stops at RC25.
    let r = run(&item(
        "Legendary Treasures: Radiant Collections",
        "Purrloin",
        "RC30",
        Variant::Holofoil,
    ));
    assert!(has(&r, FlagCode::RadiantNumber));
    assert!(r.card.is_none());
    // RC in a set without one.
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "RC6",
        Variant::Holofoil,
    ));
    assert!(has(&r, FlagCode::RadiantNumber));
    // A Radiant Collection line without an RC number.
    let r = run(&item(
        "Generations: Radiant Collection",
        "Snorunt",
        "7",
        Variant::Holofoil,
    ));
    assert!(has(&r, FlagCode::RadiantNumber));
    // In range: no flag.
    let r = run(&item(
        "Generations: Radiant Collection",
        "Snorunt",
        "RC7",
        Variant::Holofoil,
    ));
    assert!(!has(&r, FlagCode::RadiantNumber));
}

#[test]
fn promo_numbers_are_flagged() {
    let r = run(&item("XY Promos", "Yveltal EX", "XY150", Variant::Holofoil));
    assert!(has(&r, FlagCode::PromoVsSet));
    assert!(r.flags[0].message.contains("XY150"));
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre EX",
        "6",
        Variant::Holofoil,
    ));
    assert!(!has(&r, FlagCode::PromoVsSet));
}

#[test]
fn name_mismatch_on_an_id_hit() {
    // One word short of the real name: the plain Kyogre is not the EX.
    let r = run(&item(
        "Double Crisis",
        "Team Aqua's Kyogre",
        "6",
        Variant::Holofoil,
    ));
    assert_eq!(
        r.card.as_ref().unwrap().card_id,
        "dc1-6",
        "the number wins, the flag says why"
    );
    assert_eq!(codes(&r), vec![FlagCode::NameMismatch]);
    assert!(r.flags[0].message.contains("Team Aqua's Kyogre EX"));
}

#[test]
fn every_flag_message_is_one_sentence() {
    let samples = [
        run(&item(
            "Double Crisis",
            "Team Magma's Aggron (JP)",
            "RC6",
            Variant::FirstEdition,
        )),
        run(&item(
            "Base Set (Unlimited)",
            "Charmeleon",
            "24",
            Variant::Normal,
        )),
        run(&item(
            "XY Promos",
            "Yveltal EX",
            "XY150",
            Variant::ReverseHolofoil,
        )),
        run(&item(
            "Double Crisis",
            "Team Aqua's Kyogre EX",
            "99",
            Variant::Holofoil,
        )),
        run(&item(
            "Base Set (Unlimited)",
            "Charmeleon",
            "24",
            Variant::Holofoil,
        )),
    ];
    for r in &samples {
        for f in &r.flags {
            let m = f.message.trim();
            assert!(m.ends_with('.'), "{m}");
            assert_eq!(m.matches(". ").count(), 0, "one sentence: {m}");
        }
    }
}

#[test]
fn price_checks() {
    let r = run(&item(
        "Black and White Promos",
        "Reshiram (BW04)",
        "BW04",
        Variant::Holofoil,
    ));
    let mut it = item(
        "Black and White Promos",
        "Reshiram (BW04)",
        "BW04",
        Variant::Holofoil,
    );
    it.collectr_price = Some(75.0);
    let card = r.card.as_ref();

    let f = price_flags(&it, card, Some(5.0));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, FlagCode::PriceDisagrees);
    assert!(f[0].message.contains("$75.00") && f[0].message.contains("$5.00"));
    // Exactly 3x is not "more than 3x"; the other direction counts too.
    assert!(price_flags(&it, card, Some(75.0 / 3.0)).is_empty());
    assert_eq!(
        price_flags(&it, card, Some(300.0))[0].code,
        FlagCode::PriceDisagrees
    );
    assert!(price_flags(&it, card, Some(60.0)).is_empty());
    // One side missing: nothing to compare, but a price exists.
    assert!(price_flags(&it, card, None).is_empty());
    it.collectr_price = None;
    assert!(price_flags(&it, card, Some(5.0)).is_empty());
    // Neither: no price.
    let f = price_flags(&it, None, None);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, FlagCode::NoPrice);
    // Zero counts as no price (Collectr writes 0 when it has none).
    it.collectr_price = Some(0.0);
    assert_eq!(price_flags(&it, None, Some(0.0))[0].code, FlagCode::NoPrice);
}

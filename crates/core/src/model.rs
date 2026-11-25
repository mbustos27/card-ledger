//! The words the whole ledger shares: a card's condition and its printing (variant).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A physical card's condition, judged by the owner. `Unchecked` is the starting state for every
/// imported card: the ledger never assumes Near Mint, because scanner apps mark everything NM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Condition {
    Unchecked,
    NearMint,
    LightlyPlayed,
    ModeratelyPlayed,
    HeavilyPlayed,
    Damaged,
}

impl Condition {
    pub const ALL: [Condition; 6] = [
        Condition::Unchecked,
        Condition::NearMint,
        Condition::LightlyPlayed,
        Condition::ModeratelyPlayed,
        Condition::HeavilyPlayed,
        Condition::Damaged,
    ];

    /// Short code stored in the database and shown in the window.
    pub fn code(self) -> &'static str {
        match self {
            Condition::Unchecked => "unchecked",
            Condition::NearMint => "NM",
            Condition::LightlyPlayed => "LP",
            Condition::ModeratelyPlayed => "MP",
            Condition::HeavilyPlayed => "HP",
            Condition::Damaged => "DMG",
        }
    }

    /// Parses a code (`NM`, `lp`, `unchecked`, ...). Long names are accepted too.
    pub fn parse(s: &str) -> Option<Condition> {
        let t = s.trim().to_ascii_lowercase();
        Some(match t.as_str() {
            "unchecked" | "" => Condition::Unchecked,
            "nm" | "near mint" => Condition::NearMint,
            "lp" | "lightly played" => Condition::LightlyPlayed,
            "mp" | "moderately played" => Condition::ModeratelyPlayed,
            "hp" | "heavily played" => Condition::HeavilyPlayed,
            "dmg" | "damaged" => Condition::Damaged,
            _ => return None,
        })
    }
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// The printing of a card, as Collectr's "Variance" column names it. These match the TCGplayer
/// price buckets the price sources use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Variant {
    Normal,
    Holofoil,
    ReverseHolofoil,
    FirstEdition,
    FirstEditionHolofoil,
    Unlimited,
    UnlimitedHolofoil,
}

impl Variant {
    pub const ALL: [Variant; 7] = [
        Variant::Normal,
        Variant::Holofoil,
        Variant::ReverseHolofoil,
        Variant::FirstEdition,
        Variant::FirstEditionHolofoil,
        Variant::Unlimited,
        Variant::UnlimitedHolofoil,
    ];

    /// Collectr's spelling, which is also the database spelling.
    pub fn label(self) -> &'static str {
        match self {
            Variant::Normal => "Normal",
            Variant::Holofoil => "Holofoil",
            Variant::ReverseHolofoil => "Reverse Holofoil",
            Variant::FirstEdition => "1st Edition",
            Variant::FirstEditionHolofoil => "1st Edition Holofoil",
            Variant::Unlimited => "Unlimited",
            Variant::UnlimitedHolofoil => "Unlimited Holofoil",
        }
    }

    /// Short code used in canonical card ids (`bw11-RC13/holo`).
    pub fn code(self) -> &'static str {
        match self {
            Variant::Normal => "normal",
            Variant::Holofoil => "holo",
            Variant::ReverseHolofoil => "reverse",
            Variant::FirstEdition => "1st",
            Variant::FirstEditionHolofoil => "1st-holo",
            Variant::Unlimited => "unlimited",
            Variant::UnlimitedHolofoil => "unlimited-holo",
        }
    }

    pub fn parse(s: &str) -> Option<Variant> {
        let t = s.trim();
        Variant::ALL
            .into_iter()
            .find(|v| v.label().eq_ignore_ascii_case(t) || v.code().eq_ignore_ascii_case(t))
    }

    /// True for any foil printing (holo, reverse holo, 1st/Unlimited holo).
    pub fn is_foil(self) -> bool {
        matches!(
            self,
            Variant::Holofoil
                | Variant::ReverseHolofoil
                | Variant::FirstEditionHolofoil
                | Variant::UnlimitedHolofoil
        )
    }
}

impl fmt::Display for Variant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condition_codes_round_trip() {
        for c in Condition::ALL {
            assert_eq!(Condition::parse(c.code()), Some(c));
        }
        assert_eq!(Condition::parse("Near Mint"), Some(Condition::NearMint));
        assert_eq!(Condition::parse("mint"), None);
    }

    #[test]
    fn variant_labels_round_trip() {
        for v in Variant::ALL {
            assert_eq!(Variant::parse(v.label()), Some(v));
            assert_eq!(Variant::parse(v.code()), Some(v));
        }
        assert_eq!(
            Variant::parse("reverse holofoil"),
            Some(Variant::ReverseHolofoil)
        );
        assert_eq!(Variant::parse("Cosmos Holo"), None);
    }
}

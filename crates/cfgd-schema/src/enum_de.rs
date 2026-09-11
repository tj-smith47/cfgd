//! Shared machinery for case-insensitive deserialization of string-valued
//! config enums.
//!
//! Config enums serialize to canonical PascalCase (or the enum's `rename_all`
//! token) but should *parse* leniently: a user writing `driftPolicy: notifyonly`
//! or `format: yaml` in `cfgd.yaml` means the same unambiguous value as the
//! canonical token. Making the leniency intrinsic to the *type* (a manual
//! `Deserialize` impl) — rather than per-field `deserialize_with` — guarantees
//! it applies everywhere the enum is used: nested structs, `Vec<E>`,
//! `Option<E>`, `HashMap<_, E>`. A new field can never silently lack it.

/// Generate case-insensitive [`std::str::FromStr`] and `serde::Deserialize`
/// impls for a string-valued config enum, plus the variant list and canonical
/// spellings derived from the same tokens.
///
/// `$token` must equal the enum's serde token (the variant name, or its
/// `#[serde(rename)]` / `rename_all` form). Matching is ASCII-case-insensitive,
/// so every casing of every variant parses while unknown values still error via
/// `unknown_variant`. The `Serialize` derive is left untouched, so output stays
/// canonical and round-trips remain stable.
///
/// `FromStr` is how a value already read as a plain string — one a device
/// reported, one another component wrote — reaches the enum, and it is the same
/// matcher `Deserialize` runs, so a document and a stored word cannot be read
/// two different ways.
///
/// The expansion reaches serde through `$crate::serde`, so an invoking crate
/// needs no `serde` of its own in scope.
///
/// `ALL` and `as_str` come from this one token list too, so the parser, the
/// published schemas, and any rendered spelling cannot drift apart: adding a
/// variant here updates all three at once, and forgetting to is a compile error
/// rather than a silently stale list.
#[macro_export]
macro_rules! case_insensitive_enum {
    ($name:ty { $($token:literal => $variant:path),+ $(,)? }) => {
        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$($variant),+];

            /// Canonical spelling — what cfgd serializes and what the published
            /// editor schemas offer.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $($variant => $token),+
                }
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $crate::UnknownVariant;

            fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
                $(
                    if s.eq_ignore_ascii_case($token) {
                        return ::core::result::Result::Ok($variant);
                    }
                )+
                ::core::result::Result::Err($crate::UnknownVariant::new(s, &[$($token),+]))
            }
        }

        impl<'de> $crate::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
            where
                D: $crate::serde::Deserializer<'de>,
            {
                let s = <::std::string::String as $crate::serde::Deserialize>::deserialize(
                    deserializer,
                )?;
                <Self as ::core::str::FromStr>::from_str(&s).map_err(|_| {
                    <D::Error as $crate::serde::de::Error>::unknown_variant(&s, &[$($token),+])
                })
            }
        }
    };
}

/// The refusal a string-valued config enum answers a value none of its variants
/// spell with.
///
/// The message names the value as written and every token that would have been
/// accepted, so a caller reporting it needs no second copy of the variant list.
#[derive(Debug, thiserror::Error)]
#[error("'{value}' is not one of: {accepted}")]
pub struct UnknownVariant {
    value: String,
    accepted: String,
}

impl UnknownVariant {
    /// Build the refusal from the value read and the tokens the enum accepts.
    #[doc(hidden)]
    #[must_use]
    pub fn new(value: &str, accepted: &[&str]) -> Self {
        Self {
            value: value.to_string(),
            accepted: accepted.join(", "),
        }
    }
}

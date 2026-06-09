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

/// Generate a case-insensitive `serde::Deserialize` impl for a string-valued
/// config enum.
///
/// `$token` must equal the enum's serde token (the variant name, or its
/// `#[serde(rename)]` / `rename_all` form). Matching is ASCII-case-insensitive,
/// so every casing of every variant parses while unknown values still error via
/// `unknown_variant`. The `Serialize` derive is left untouched, so output stays
/// canonical and round-trips remain stable.
macro_rules! case_insensitive_enum {
    ($name:ty { $($token:literal => $variant:expr),+ $(,)? }) => {
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let s = <::std::string::String as serde::Deserialize>::deserialize(deserializer)?;
                $(
                    if s.eq_ignore_ascii_case($token) {
                        return ::core::result::Result::Ok($variant);
                    }
                )+
                ::core::result::Result::Err(<D::Error as serde::de::Error>::unknown_variant(
                    &s,
                    &[$($token),+],
                ))
            }
        }
    };
}

//! Shared wire conventions for every type in this crate (management-api §1.1).
//!
//! - **Field names** are `snake_case`; `/v1/settings` alone uses `settings.json`'s own names.
//! - **Times** are `chrono::DateTime<Utc>` (RFC 3339 UTC). **Durations** are `*_ms: u64`.
//! - **IDs** are `String`, snowflakes included.
//! - **Nullable vs optional.** A field the spec writes as `x|null` is an `Option<T>` that is
//!   always sent (`null` when empty) and marked `#[schema(required = true)]`, so generated
//!   clients see "always present, may be null". A field the spec writes as `x?` is an
//!   `Option<T>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
//! - **Integer widths.** Counts and positions are `u32`; token counts, bytes and `*_ms` are `u64`.
//! - **Closed string sets** use [`wire_enum!`], which adds the catch-all §1.1 requires and keeps
//!   it out of the schema. Where `unknown` is a real value of the set (`runs::RunOutcome`,
//!   `supervisor::DaemonReachability`), the enum is hand-written instead: `Unknown` is both the
//!   real value and the catch-all, and it is in the schema.
//! - **Tagged unions** (`#[serde(tag = "kind")]`) carry a catch-all unit variant too. Their
//!   schema is derived from a private mirror enum without it, in a `mod schema`, with a test that
//!   every known variant round-trips through the mirror (see `turns::TurnEvent`). A variant's own
//!   field never reuses the tag's name; it becomes `<variant>_kind` (`notice_kind`).
//! - **Content** (prompts, reply text, job bodies, personas, attachments) never appears in a
//!   derived `Debug`: such types get a hand-written `Debug` that prints only sizes (architecture
//!   §2).
//! - Each resource module declares `pub(crate) struct Doc` with `#[derive(utoipa::OpenApi)]`
//!   listing its schemas, and [`crate::openapi`] merges it.

use serde::Serialize;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};

/// Defines a unit enum sent as a string: each variant names its wire value, and a trailing
/// `Unknown` catch-all parses values added later in v1. `KNOWN` lists every other variant, and
/// the schema is exactly that list.
///
/// ```ignore
/// wire_enum! {
///     /// Whether a Daemon is healthy.
///     pub enum Health {
///         Ok = "ok",
///         Degraded = "degraded",
///     }
/// }
/// ```
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$vmeta:meta])* $variant:ident = $wire:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            serde::Serialize, serde::Deserialize,
        )]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $wire)] $variant,)*
            /// A value added later in v1 that this build doesn't know (management-api §1.1).
            /// Never sent, and not in the schema.
            #[serde(rename = "unknown", other)]
            Unknown,
        }

        impl $name {
            /// Every value except `Unknown`, in declaration order.
            pub const KNOWN: &[$name] = &[$($name::$variant,)*];
        }

        impl utoipa::PartialSchema for $name {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                $crate::wire::string_enum(Self::KNOWN)
            }
        }

        impl utoipa::ToSchema for $name {}
    };
}

pub(crate) use wire_enum;

/// A string schema listing the serde names of `values`.
pub(crate) fn string_enum<T: Serialize>(values: &[T]) -> RefOr<Schema> {
    let names = values
        .iter()
        .map(|v| serde_json::to_value(v).expect("unit variant"));
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(names))
        .into()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use utoipa::PartialSchema;

    wire_enum! {
        /// A test set.
        pub enum Colour {
            Red = "red",
            DarkBlue = "dark_blue",
        }
    }

    #[test]
    fn known_values_round_trip_and_unknown_ones_parse() {
        assert_eq!(Colour::KNOWN, [Colour::Red, Colour::DarkBlue]);
        assert_eq!(serde_json::to_value(Colour::DarkBlue).unwrap(), "dark_blue");
        assert_eq!(
            serde_json::from_value::<Colour>(json!("red")).unwrap(),
            Colour::Red
        );
        assert_eq!(
            serde_json::from_value::<Colour>(json!("green")).unwrap(),
            Colour::Unknown
        );
        assert_eq!(serde_json::to_value(Colour::Unknown).unwrap(), "unknown");
    }

    #[test]
    fn the_schema_lists_only_known_values() {
        let schema = serde_json::to_value(Colour::schema()).unwrap();
        assert_eq!(
            schema,
            json!({"type": "string", "enum": ["red", "dark_blue"]})
        );
    }
}

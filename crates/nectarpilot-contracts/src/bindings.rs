//! Deterministic Rust-to-TypeScript contract export.

use specta::TypeCollection;

use crate::{CommandEnvelope, Detection, EventEnvelope, NormalizedRegion, Profile};

#[must_use]
pub fn type_collection() -> TypeCollection {
    let mut types = TypeCollection::default();
    // Register public roots. Specta recursively includes every profile,
    // command/event, state, and result dependency.
    types.register::<Profile>();
    types.register::<CommandEnvelope>();
    types.register::<EventEnvelope>();
    types.register::<Detection<String>>();
    types.register::<NormalizedRegion>();
    types
}

pub fn typescript() -> Result<String, specta_typescript::ExportError> {
    // JSON carries counters as numbers. They are operational counters/durations
    // bounded far below JavaScript's exact-integer ceiling.
    let generated = specta_typescript::Typescript::default()
        .bigint(specta_typescript::BigIntExportBehavior::Number)
        .export(&type_collection())?;
    let normalized = generated
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("{}\n", normalized.trim_end()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn checked_in_typescript_is_current() {
        let generated = super::typescript().expect("TypeScript export");
        let checked_in = include_str!("../bindings/generated.ts");
        assert_eq!(
            checked_in, generated,
            "run `cargo run -p nectarpilot-contracts --bin export-typescript`"
        );
    }
}

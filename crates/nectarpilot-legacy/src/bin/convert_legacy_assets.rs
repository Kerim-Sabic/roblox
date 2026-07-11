use std::path::PathBuf;

use nectarpilot_legacy::{AssetKind, generate_asset_catalog};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let legacy_root = arguments.next().map_or_else(
        || std::env::current_dir().expect("current directory is available"),
        PathBuf::from,
    );
    let assets_root = arguments
        .next()
        .map_or_else(|| legacy_root.join("assets"), PathBuf::from);
    if arguments.next().is_some() {
        return Err("usage: convert_legacy_assets [legacy-root] [assets-root]".into());
    }

    let routes =
        generate_asset_catalog(&legacy_root, &assets_root.join("routes"), AssetKind::Route)?;
    let patterns = generate_asset_catalog(
        &legacy_root,
        &assets_root.join("patterns"),
        AssetKind::Pattern,
    )?;
    println!(
        "cataloged {} routes ({} safe DSL) and {} patterns ({} safe DSL)",
        routes.total_files, routes.safe_dsl_files, patterns.total_files, patterns.safe_dsl_files
    );
    Ok(())
}

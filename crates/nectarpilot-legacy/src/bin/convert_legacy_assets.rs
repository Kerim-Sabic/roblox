use std::path::PathBuf;

use nectarpilot_legacy::{
    AssetKind, generate_asset_catalog, validate_detector_templates, write_detector_catalog,
    write_support_catalog,
};

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
    let support_directory = assets_root.join("legacy-support");
    std::fs::create_dir_all(&support_directory)?;
    let support = write_support_catalog(
        &legacy_root,
        &support_directory.join("_legacy-manifest.yaml"),
    )?;
    let detector_directory = assets_root.join("detectors");
    std::fs::create_dir_all(&detector_directory)?;
    let detectors = write_detector_catalog(
        &legacy_root,
        &detector_directory.join("_legacy-manifest.yaml"),
    )?;
    let detector_report = validate_detector_templates(&legacy_root)?;
    println!(
        "cataloged {} routes ({} safe DSL), {} patterns ({} safe DSL), and {} pinned support files",
        routes.total_files,
        routes.safe_dsl_files,
        patterns.total_files,
        patterns.safe_dsl_files,
        support.entries.len()
    );
    println!(
        "detector templates: {} files, {} decoded images, {} inline templates, {} script references, {} missing, {} corrupt",
        detectors.total_files,
        detectors.image_files,
        detectors.inline_templates,
        detector_report.references,
        detector_report.missing.len(),
        detector_report.corrupt.len()
    );
    if !detector_report.is_clean() {
        for problem in detector_report
            .missing
            .iter()
            .chain(detector_report.corrupt.iter())
        {
            eprintln!("DETECTOR PROBLEM: {problem}");
        }
        return Err("detector template validation failed".into());
    }
    Ok(())
}

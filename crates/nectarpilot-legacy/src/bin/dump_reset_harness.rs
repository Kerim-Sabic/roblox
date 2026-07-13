//! Prints the generated builtin reset/convert harness for manual review.

use nectarpilot_legacy::{HarnessSettings, generate_reset_script};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::fs::canonicalize(std::env::current_dir()?)?;
    let script = generate_reset_script(&root, &HarnessSettings::default(), 30)?;
    print!("{script}");
    Ok(())
}

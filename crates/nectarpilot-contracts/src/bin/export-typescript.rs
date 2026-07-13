use std::{env, error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let destination = env::args_os().nth(1).map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("bindings")
                .join("generated.ts")
        },
        PathBuf::from,
    );
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&destination, nectarpilot_contracts::bindings::typescript()?)?;
    println!("wrote {}", destination.display());
    Ok(())
}

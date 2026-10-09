//! Release gate: the key built into clients must verify every published file.
use anyhow::{Context, Result};
use std::path::Path;
fn verify_directory(path: &Path, key: &str) -> Result<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?.path();
        if entry.is_dir() {
            count += verify_directory(&entry, key)?;
        } else if entry.extension().is_some_and(|s| s == "sig") {
            let package = entry.with_extension("");
            beacon_updater::verify_package(key, &package, std::fs::read_to_string(&entry)?.trim())
                .with_context(|| format!("Could not verify {}", package.display()))?;
            count += 1;
        }
    }
    Ok(count)
}
fn main() -> Result<()> {
    let directory = std::env::args_os()
        .nth(1)
        .context("Usage: verify-update ARTIFACTS_DIRECTORY")?;
    let key = std::env::var("BEACON_UPDATE_PUBLIC_KEY")
        .context("BEACON_UPDATE_PUBLIC_KEY is required")?;
    let count = verify_directory(Path::new(&directory), &key)?;
    anyhow::ensure!(count > 0, "No signed packages were found");
    println!("Verified {count} signed update assets.");
    Ok(())
}

use std::{env, path::PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let resource = root.join("../../assets/packaging/beacon.rc");
    let icon = root.join("../../assets/app-icon/beacon.ico");
    println!("cargo:rerun-if-changed={}", resource.display());
    println!("cargo:rerun-if-changed={}", icon.display());

    // Check the target, since the build script can run on a non-Windows host.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // NSIS shortcuts take their icon from the executable. Fail the build if
    // the resource compiler is unavailable instead of shipping a blank icon.
    let icon_definition = format!(
        "BEACON_ICON_PATH=\"{}\"",
        icon.to_string_lossy().replace('\\', "/")
    );
    embed_resource::compile_for(resource, ["beacon"], [icon_definition])
        .manifest_required()
        .expect("failed to embed the Windows application icon");
}

fn main() {
    println!("cargo:rerun-if-changed=assets/clipserve.rc");
    println!("cargo:rerun-if-changed=assets/clipserve.ico");
    println!("cargo:rerun-if-changed=assets/clipserve.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = format!(
        "CLIPSERVE_FILE_VERSION={},{},{},0",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    );
    let display_version = format!("CLIPSERVE_VERSION=\"{}\"", env!("CARGO_PKG_VERSION"));
    embed_resource::compile_for(
        "assets/clipserve.rc",
        ["clipserve"],
        embed_resource::ParamsMacrosAndIncludeDirs([version, display_version], ["assets"]),
    )
    .manifest_required()
    .expect("Could not embed the ClipServe executable resources");
}

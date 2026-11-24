fn main() {
    println!("cargo:rerun-if-changed=assets/clipserve.ico");
    println!("cargo:rerun-if-changed=assets/clipserve.manifest");
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("assets/clipserve.ico")
        .set_manifest_file("assets/clipserve.manifest")
        .set("ProductName", "ClipServe")
        .set("FileDescription", "ClipServe")
        .compile()
        .expect("Could not embed the ClipServe executable icon");
}

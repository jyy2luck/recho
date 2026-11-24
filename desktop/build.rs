fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icons/recho.rc");
        println!("cargo:rerun-if-changed=assets/icons/recho.ico");
        winresource::WindowsResource::new()
            .set_resource_file("assets/icons/recho.rc")
            .compile_for("recho")
            .expect("failed to embed the Recho Windows icon");
    }
}

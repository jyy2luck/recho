fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icons/recho.ico");
        // Task Manager uses the executable's icon and file description for the process group.
        winresource::WindowsResource::new()
            .set_icon("assets/icons/recho.ico")
            .set("InternalName", "recho")
            .set("OriginalFilename", "recho.exe")
            .compile_for("recho")
            .expect("failed to embed Recho's Windows resources");
    }
}

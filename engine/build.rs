fn main() {
    println!("cargo:rerun-if-changed=src/application_macos.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/application_macos.c")
            .compile("recho_application");
    }
}

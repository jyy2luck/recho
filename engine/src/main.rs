#[tokio::main]
async fn main() {
    let helper = std::env::args().next().is_some_and(|path| {
        std::path::Path::new(&path)
            .file_name()
            .is_some_and(|name| name == "mcp-server")
    });
    let result =
        recho_core::run(helper || std::env::args().nth(1).as_deref() == Some("--mcp")).await;
    if let Err(error) = result {
        eprintln!("Engine startup failed: {error}");
        std::process::exit(1);
    }
}

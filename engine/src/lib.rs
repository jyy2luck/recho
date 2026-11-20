mod api_scripts;
mod application;
mod capture_filters;
mod certificates;
mod documents;
mod grpc;
mod grpc_client;
mod identity;
mod local_network;
mod mcp;
mod protocol_client;
mod python;
mod raw_client;
mod rules;
mod server;
mod settings;
mod storage;
mod system_proxy;
mod ui_bridge;
mod websocket;
mod wire;

mod android_vpn;

pub async fn run(mcp: bool) -> Result<(), String> {
    let result = if mcp {
        mcp::run().await
    } else {
        server::run().await
    };
    result.map_err(|error| error.to_string())
}

pub use server::native;

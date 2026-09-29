// Generates the gRPC service and messages from proto/ (see proto/chaindeal/v1/ledger.proto).
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fds = protox::compile(["chaindeal/v1/ledger.proto"], ["../proto"])?;
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    // Also keep the descriptor set, for gRPC server reflection (grpcurl).
    std::fs::write(out.join("ledger_descriptor.bin"), prost::Message::encode_to_vec(&fds))?;
    tonic_prost_build::configure().build_client(true).build_server(true).compile_fds(fds)?;
    println!("cargo:rerun-if-changed=../proto");
    Ok(())
}

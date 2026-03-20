fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true) // client needed for integration tests
        .compile_protos(&["proto/csi.proto"], &["proto/"])?;
    Ok(())
}

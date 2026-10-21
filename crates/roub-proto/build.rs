//! Compiles the protobuf contract with a vendored `protoc`, so building needs no system
//! protobuf installation on Linux or Windows.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = tonic_prost_build::Config::new();
    config.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure().compile_with_config(
        config,
        &["proto/roub/v1/roub.proto"],
        &["proto"],
    )?;
    Ok(())
}

//! Build script: generates Rust gRPC + protobuf types from `proto/racli.proto` into `OUT_DIR` for `tonic::include_proto!`.

/// Runs `tonic-prost` codegen for the Racli `.proto` file before the crate compiles.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::compile_protos("proto/racli.proto")?;
    Ok(())
}

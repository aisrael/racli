//! Build script: generates Rust prost message types from `proto/racli.proto` into `OUT_DIR` for `include!`.

/// Runs `prost-build` codegen for the Racli `.proto` file before the crate compiles.
fn main() -> std::io::Result<()> {
    prost_build::compile_protos(&["proto/racli.proto"], &["proto"])
}

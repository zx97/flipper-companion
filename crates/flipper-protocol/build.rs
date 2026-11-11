//! Compiles the vendored Flipper Zero protobuf definitions.
//!
//! `protox` is a pure-Rust protobuf compiler, so building this crate does not
//! require a `protoc` binary on the host.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");

    let descriptors = protox::compile(["proto/flipper.proto"], ["proto"])?;
    prost_build::Config::new().compile_fds(descriptors)?;

    Ok(())
}

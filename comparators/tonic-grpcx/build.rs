//! Compile echo.proto via protox (pure-Rust, no system
//! protoc) + tonic-build.

fn main() {
    let fds = protox::compile(["proto/echo.proto", "proto/probe2.proto"], ["proto/"])
        .expect("protox compile");
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(fds)
        .expect("tonic-build");
    println!("cargo:rerun-if-changed=proto/echo.proto");
    println!("cargo:rerun-if-changed=proto/probe2.proto");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/command.proto");
    // Use a vendored protoc so Windows/macOS/Android CI does not need a system
    // protobuf install.
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: build scripts are single-threaded here.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }
    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&["proto/command.proto"], &["proto"])?;
    Ok(())
}

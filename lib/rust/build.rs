use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(protoc_path) = protoc_bin_vendored::protoc_bin_path() {
        if let Some(s) = protoc_path.to_str() {
            env::set_var("PROTOC", s);
        }
    }

    let proto_files = ["../../server/proto/syncpond.proto"];
    let includes = ["../../server/proto"];

    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile(&proto_files, &includes)?;

    println!("cargo:rerun-if-changed=../../server/proto/syncpond.proto");
    Ok(())
}

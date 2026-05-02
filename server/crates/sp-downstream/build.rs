use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(protoc_path) = protoc_bin_vendored::protoc_bin_path() {
        if let Some(s) = protoc_path.to_str() {
            env::set_var("PROTOC", s);
        }
    }

    let _out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let proto_files = ["proto/downstream.proto"];
    let includes = ["proto"];

    tonic_build::configure()
        .build_client(false)
        .build_server(false)
        .compile(&proto_files, &includes)?;

    println!("cargo:rerun-if-changed=proto/downstream.proto");
    Ok(())
}

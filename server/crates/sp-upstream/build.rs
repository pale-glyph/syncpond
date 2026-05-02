use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(protoc_path) = protoc_bin_vendored::protoc_bin_path() {
        if let Some(s) = protoc_path.to_str() {
            env::set_var("PROTOC", s);
        }
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let proto_files = ["proto/syncpond.proto"];
    let includes = ["proto"];

    tonic_build::configure()
        .file_descriptor_set_path(out_dir.join("syncpond_descriptor.pb"))
        .build_server(true)
        .compile(&proto_files, &includes)?;

    println!("cargo:rerun-if-changed=proto/syncpond.proto");
    Ok(())
}

use std::{env, fs, io::Write, path};

fn main() {
    let fuels_accounts_dir = path::PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    let workspace_dir = &fuels_accounts_dir
        .ancestors()
        .nth(2)
        .expect("failed to get workspace directory");
    let workspace_manifest = workspace_dir.join("Cargo.toml");
    assert!(
        workspace_manifest.exists(),
        "couldn't find workspace Cargo.toml"
    );

    println!("cargo:rerun-if-changed={}", workspace_manifest.display());
    println!("cargo:rerun-if-changed=build.rs");

    let out_path = fuels_accounts_dir.join("fuel-core-version");
    let out_file = fs::File::create(out_path).expect("failed to create fuel-core-version file");
    write!(&out_file, "{}", fuel_core::VERSION).expect("failed to write fuel-core-version file");
}

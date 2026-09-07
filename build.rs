use sha2::{Digest, Sha256};
use std::path::Path;
fn sources(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}
fn main() {
    let mut files = vec!["Cargo.toml".into(), "Cargo.lock".into(), "build.rs".into()];
    sources(Path::new("src"), &mut files);
    files.sort();
    let mut hash = Sha256::new();
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(std::fs::read(path).unwrap());
    }
    println!("cargo:rustc-env=IRONMEM_SOURCE_SHA={:x}", hash.finalize());
}

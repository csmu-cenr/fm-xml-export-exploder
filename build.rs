use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=version_timestamp.txt");
    let timestamp =
        fs::read_to_string("version_timestamp.txt").expect("Cannot read version_timestamp.txt");
    let timestamp = timestamp.trim();
    assert!(
        !timestamp.is_empty() && !timestamp.contains(['\n', '\r']),
        "version_timestamp.txt must contain one nonempty line"
    );
    println!("cargo:rustc-env=VERSION_TIMESTAMP={timestamp}");
}

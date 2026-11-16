fn main() {
    // Year shown in Settings > About.
    let secs = std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    println!("cargo:rustc-env=BUILD_YEAR={}", 1970 + secs / 31_556_952);
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    tauri_build::build()
}

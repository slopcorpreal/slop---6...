use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CHRONOSUB_COMMIT_COUNT");
    println!("cargo:rerun-if-changed=.git/HEAD");

    let commit_count = std::env::var("CHRONOSUB_COMMIT_COUNT").ok().or_else(|| {
        Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }).or_else(|| {
        std::env::var("CARGO_PKG_VERSION")
            .ok()
            .and_then(|v| v.split('.').nth(1).map(str::to_string))
    }).unwrap_or_else(|| "0".to_string());

    println!("cargo:rustc-env=CHRONOSUB_COMMIT_COUNT={commit_count}");
}

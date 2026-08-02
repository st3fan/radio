//! Embeds the build flavor: official release builds (the release workflow
//! sets RADIOD_RELEASE_BUILD) get an empty dev hash and the banner shows
//! the version; every other build — dev deploys included — carries the
//! short git hash so the banner can say RADIO DEV (abc1234).

fn main() {
    println!("cargo:rerun-if-env-changed=RADIOD_RELEASE_BUILD");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    let dev_hash = if std::env::var("RADIOD_RELEASE_BUILD").is_ok() {
        String::new()
    } else {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    };
    println!("cargo:rustc-env=RADIOD_DEV_HASH={dev_hash}");
}

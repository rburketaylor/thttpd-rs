fn main() {
    // Linux and several BSDs provide crypt(3) in a separate library. Apple
    // platforms expose it through libSystem and fail when passed -lcrypt.
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(
        target.as_str(),
        "linux" | "freebsd" | "netbsd" | "openbsd" | "dragonfly"
    ) {
        println!("cargo:rustc-link-lib=crypt");
    }
}

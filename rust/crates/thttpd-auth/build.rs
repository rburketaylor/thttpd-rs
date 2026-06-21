fn main() {
    // crypt(3) lives in libcrypt (a separate library) on Linux and several
    // BSDs; Apple platforms expose it through libSystem and fail when passed
    // -lcrypt. This build script links libcrypt where needed so the
    // `unsafe extern "C" { crypt }` FFI in src/lib.rs resolves.
    //
    // This is one of the three audited OS/FFI boundaries documented in
    // docs/SECURITY_NOTES.md. It is intentionally isolated in its own crate so
    // that cargo-geiger can honestly report thttpd-http (the request-parsing
    // crate) as unsafe-free.
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(
        target.as_str(),
        "linux" | "freebsd" | "netbsd" | "openbsd" | "dragonfly"
    ) {
        println!("cargo:rustc-link-lib=crypt");
    }
}

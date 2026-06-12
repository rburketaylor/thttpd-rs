fn main() {
    // Link against libcrypt for crypt(3) (used by auth.rs)
    println!("cargo:rustc-link-lib=crypt");
}

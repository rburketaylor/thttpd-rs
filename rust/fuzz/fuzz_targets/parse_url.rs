// Fuzz target: URL path normalization. `normalize_path` returns Option<String>
// and must not panic on garbage bytes (path traversal attempts, non-UTF8, etc.).
//
// normalize_path signature (rust/crates/thttpd-http/src/url.rs:38):
//   pub fn normalize_path(path: &str) -> Option<String>
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = thttpd_http::url::normalize_path(&String::from_utf8_lossy(data));
});

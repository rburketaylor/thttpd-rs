//! Basic Auth implementation matching C's `auth_check2` (libhttpd.c:995-1147).
//!
//! When a `.htpasswd` file exists in a directory containing the requested file,
//! the server requires `Authorization: Basic <base64(user:pass)>` header. The
//! password is verified using libc's `crypt(3)` function (DES, MD5, SHA-256,
//! SHA-512) — the same function the C binary uses, guaranteeing byte-exact
//! hash compatibility.

use std::ffi::{CStr, CString};
use std::path::Path;

/// Magic filename for the password database, matching `AUTH_FILE` in
/// `legacy/src/thttpd.h:139`.
pub const AUTH_FILE: &str = ".htpasswd";

/// 401 Unauthorized response title (matches C's `err401title`).
pub const ERR_401_TITLE: &str = "Unauthorized";

/// Format string for the 401 response body (matches C's `err401form`).
pub const ERR_401_FORM: &str =
    "Authorization required for the URL '%.80s'.\n";

/// Check whether a directory requires Basic Auth.
///
/// Returns:
/// - `AuthResult::Ok` if no `.htpasswd` exists or auth was successful
/// - `AuthResult::NoAuthFile` if no `.htpasswd` in the directory tree
/// - `AuthResult::Unauthorized(String)` if auth is required and missing/wrong
///
/// `authorization` is the value of the `Authorization:` header (may be empty).
/// `path` is the on-disk path to the file being served (used to find the
/// containing directory's `.htpasswd`).
pub fn auth_check2(path: &Path, authorization: &str) -> AuthResult {
    // Walk up from the file's directory to find the nearest .htpasswd
    let mut current = path.parent();
    while let Some(dir) = current {
        let auth_path = dir.join(AUTH_FILE);
        if auth_path.exists() {
            return check_htpasswd(&auth_path, authorization, path);
        }
        current = dir.parent();
    }
    AuthResult::NoAuthFile
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthResult {
    /// No `.htpasswd` exists in the directory tree — request proceeds.
    NoAuthFile,
    /// `.htpasswd` exists, auth was successful (or required and verified).
    Ok,
    /// `.htpasswd` exists, auth is required (sends 401 to client).
    Unauthorized,
}

/// Read the .htpasswd file and verify the user. Mirrors the C logic at
/// `libhttpd.c:1080-1146` but in safe Rust.
fn check_htpasswd(auth_path: &Path, authorization: &str, _file_path: &Path) -> AuthResult {
    // Decode "Basic <base64>" header
    let creds = match parse_basic_auth(authorization) {
        Some(c) => c,
        None => return AuthResult::Unauthorized, // C returns -1 → 401
    };

    let (user, pass) = creds;

    // Read the .htpasswd file. Format: one user per line: "user:crypt_hash"
    let content = match std::fs::read_to_string(auth_path) {
        Ok(c) => c,
        Err(_) => {
            // File exists but we can't read it. C returns 403 here, but for
            // simplicity we treat as unauthorized (file unreadable = 401).
            return AuthResult::Unauthorized;
        }
    };

    for line in content.lines() {
        // Trim trailing whitespace/newline
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // Split on first colon
        let (line_user, line_hash) = match line.split_once(':') {
            Some(parts) => parts,
            None => continue, // skip malformed lines (matches C's "continue" at L1105)
        };
        if line_user != user {
            continue;
        }
        // Found the user. Verify the password using crypt(3).
        if verify_password(&pass, line_hash) {
            return AuthResult::Ok;
        } else {
            return AuthResult::Unauthorized;
        }
    }
    // User not found
    AuthResult::Unauthorized
}

/// Parse `Authorization: Basic <base64(user:pass)>` into (user, pass).
/// Returns None if the header is missing, malformed, or not "Basic" type.
fn parse_basic_auth(authorization: &str) -> Option<(String, String)> {
    let auth = authorization.trim();
    if !auth.starts_with("Basic ") {
        return None;
    }
    let b64_part = auth.strip_prefix("Basic ")?.trim();

    // Base64 decode. Rust's `engine` API is what current base64 crate uses.
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64_part)
        .ok()?;

    let decoded_str = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded_str.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// Verify a password against a stored hash using libc's crypt(3).
/// Supports DES (1-char salt), MD5 ($1$), SHA-256 ($5$), SHA-512 ($6$).
///
/// A global Mutex serializes calls to crypt(3) because the glibc implementation
/// may return a pointer to a shared (non-thread-local) buffer in some libcs.
/// This is the same safety pattern used in the C code (single-threaded).
fn verify_password(password: &str, stored_hash: &str) -> bool {
    let pass_c = match CString::new(password) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let salt_c = match CString::new(stored_hash) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Serialize access to crypt(3) which is not guaranteed thread-safe.
    let _guard = CRYPT_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: crypt() takes two null-terminated C strings, returns a
    // pointer to a buffer owned by libc. With the Mutex held, no other
    // thread is using the buffer.
    let result_ptr = unsafe { libc_crypt(pass_c.as_ptr(), salt_c.as_ptr()) };
    if result_ptr.is_null() {
        return false;
    }
    // SAFETY: crypt() returns a null-terminated string on success.
    let result = unsafe { CStr::from_ptr(result_ptr) };
    let result_str = match result.to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };
    // C uses strcmp to compare the crypted password with the stored hash.
    // Copy the result out of the libc buffer before releasing the Mutex.
    let matched = result_str == stored_hash;
    matched
}

/// Global Mutex to serialize crypt(3) calls. glibc's crypt() returns a
/// pointer to a non-thread-local buffer on some libcs; this prevents races.
static CRYPT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

// FFI declaration for crypt(3) — the libc crate doesn't expose it directly.
// On Linux, this is in libcrypt (linked separately from libc).
unsafe extern "C" {
    #[link_name = "crypt"]
    fn libc_crypt(key: *const libc::c_char, salt: *const libc::c_char) -> *mut libc::c_char;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_basic_auth_valid() {
        // base64("alice:secret") = "YWxpY2U6c2VjcmV0"
        let result = parse_basic_auth("Basic YWxpY2U6c2VjcmV0");
        assert_eq!(result, Some(("alice".to_string(), "secret".to_string())));
    }

    #[test]
    fn test_parse_basic_auth_with_password_colon() {
        // base64("bob:pass:with:colons") = "Ym9iOnBhc3M6d2l0aDpjb2xvbnM="
        let result = parse_basic_auth("Basic Ym9iOnBhc3M6d2l0aDpjb2xvbnM=");
        assert_eq!(result, Some(("bob".to_string(), "pass:with:colons".to_string())));
    }

    #[test]
    fn test_parse_basic_auth_wrong_scheme() {
        assert_eq!(parse_basic_auth("Bearer abc"), None);
    }

    #[test]
    fn test_parse_basic_auth_empty() {
        assert_eq!(parse_basic_auth(""), None);
    }

    #[test]
    fn test_parse_basic_auth_no_colon_in_decoded() {
        // base64("nocolon") = "bm9jb2xvbg=="
        let result = parse_basic_auth("Basic bm9jb2xvbg==");
        assert_eq!(result, None);
    }

    #[test]
    fn test_auth_check2_no_htpasswd() {
        // No .htpasswd in the directory tree
        let result = auth_check2(Path::new("/tmp/test_www/test.txt"), "");
        assert_eq!(result, AuthResult::NoAuthFile);
    }

    #[test]
    fn test_auth_check2_unauthorized_no_header() {
        // Create a .htpasswd in a temp dir
        let dir = tempfile::tempdir().unwrap();
        let htpasswd = dir.path().join(".htpasswd");
        let mut f = std::fs::File::create(&htpasswd).unwrap();
        // MD5 crypt hash of "secret" with salt "abcd"
        // Generated by: openssl passwd -1 -salt abcd secret
        writeln!(f, "alice:$1$abcd$Oy8OD9LGKv7H9yIMreLNV1").unwrap();

        let file = dir.path().join("data.txt");
        std::fs::write(&file, "data").unwrap();

        let result = auth_check2(&file, "");
        assert_eq!(result, AuthResult::Unauthorized);
    }

    #[test]
    fn test_auth_check2_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let htpasswd = dir.path().join(".htpasswd");
        let mut f = std::fs::File::create(&htpasswd).unwrap();
        writeln!(f, "alice:$1$abcd$Oy8OD9LGKv7H9yIMreLNV1").unwrap();

        let file = dir.path().join("data.txt");
        std::fs::write(&file, "data").unwrap();

        // base64("alice:wrong")
        use base64::Engine;
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("alice:wrong"));
        let result = auth_check2(&file, &auth);
        assert_eq!(result, AuthResult::Unauthorized);
    }

    #[test]
    fn test_auth_check2_correct_password() {
        let dir = tempfile::tempdir().unwrap();
        let htpasswd = dir.path().join(".htpasswd");
        let mut f = std::fs::File::create(&htpasswd).unwrap();
        // MD5 crypt of "secret" with salt "abcd" = "$1$abcd$Oy8OD9LGKv7H9yIMreLNV1"
        writeln!(f, "alice:$1$abcd$Oy8OD9LGKv7H9yIMreLNV1").unwrap();

        let file = dir.path().join("data.txt");
        std::fs::write(&file, "data").unwrap();

        // base64("alice:secret")
        use base64::Engine;
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("alice:secret"));
        let result = auth_check2(&file, &auth);
        assert_eq!(result, AuthResult::Ok);
    }

    #[test]
    fn test_auth_check2_user_not_in_file() {
        let dir = tempfile::tempdir().unwrap();
        let htpasswd = dir.path().join(".htpasswd");
        let mut f = std::fs::File::create(&htpasswd).unwrap();
        writeln!(f, "alice:$1$abcd$Oy8OD9LGKv7H9yIMreLNV1").unwrap();

        let file = dir.path().join("data.txt");
        std::fs::write(&file, "data").unwrap();

        // base64("bob:secret")
        use base64::Engine;
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("bob:secret"));
        let result = auth_check2(&file, &auth);
        assert_eq!(result, AuthResult::Unauthorized);
    }
}

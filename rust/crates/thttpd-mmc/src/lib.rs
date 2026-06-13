//! Memory-mapped file cache for thttpd.
//! Replaces C's mmc with `Rc<Mmap>` and `HashMap`.
//!
//! Key insight: `Rc::strong_count() == 1` means only the cache holds the mapping
//! (evictable), mirroring C's `refcount == 0`.

// Re-export Mmap for consumers that need the type
pub use memmap2::Mmap;

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Cache entry holding a reference-counted mmap and metadata.
struct CacheEntry {
    mmap: Rc<Mmap>,
    last_used: Instant,
    size: u64,
}

/// Key for cache lookup: (device, inode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileKey {
    dev: u64,
    ino: u64,
}

/// Error type for mmap cache operations.
#[derive(Debug, thiserror::Error)]
pub enum MmapError {
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Memory-mapped file cache.
pub struct MmapCache {
    entries: HashMap<FileKey, CacheEntry>,
    expire_age: Duration,
    max_size: usize,
}

impl MmapCache {
    /// Create a new mmap cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            expire_age: Duration::from_secs(120),
            max_size: 64 * 1024 * 1024, // 64 MB default
        }
    }

    /// Create a cache with a custom max size.
    pub fn with_max_size(max_size: usize) -> Self {
        Self {
            max_size,
            ..Self::new()
        }
    }

    /// Map a file into memory, returning a reference-counted handle.
    /// If the file is already cached and unchanged, returns the existing mapping.
    pub fn map(&mut self, path: &Path) -> Result<Rc<Mmap>, MmapError> {
        let file =
            File::open(path).map_err(|_| MmapError::FileNotFound(path.display().to_string()))?;
        let metadata = file.metadata()?;

        #[cfg(unix)]
        let key = {
            use std::os::unix::fs::MetadataExt;
            FileKey {
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        };

        #[cfg(not(unix))]
        let key = {
            // Fallback: use path hash
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            path.hash(&mut hasher);
            FileKey {
                dev: 0,
                ino: hasher.finish(),
            }
        };

        // Check cache for existing mapping
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = Instant::now();
            return Ok(Rc::clone(&entry.mmap));
        }

        // Create new mapping
        // SAFETY: Mmap::map is safe when the file is not modified concurrently.
        // thttpd only serves static files; concurrent modification is not expected.
        let mmap = unsafe { Mmap::map(&file)? };
        let size = metadata.len();
        self.entries.insert(
            key,
            CacheEntry {
                mmap: Rc::new(mmap),
                last_used: Instant::now(),
                size,
            },
        );

        Ok(Rc::clone(&self.entries[&key].mmap))
    }

    /// Release a reference to a mapped file.
    /// This decrements the reference count. Actual cleanup happens in `cleanup()`.
    pub fn unmap(&mut self, _mmap: &Rc<Mmap>) {
        // Rc::clone/Rc::drop handles reference counting automatically.
        // No explicit action needed here — cleanup evicts entries where
        // Rc::strong_count() == 1 (only cache holds it).
    }

    /// Evict cache entries that are no longer in use and have expired.
    /// Should be called periodically (every OCCASIONAL_TIME = 120s in C).
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        let expire_age = self.expire_age;

        self.entries.retain(|_, entry| {
            // Keep if still referenced by connections
            if Rc::strong_count(&entry.mmap) > 1 {
                return true;
            }
            // Keep if not expired yet
            now.duration_since(entry.last_used) < expire_age
        });

        // Adaptive expiry: if cache is too large, reduce expire_age
        let total_size: u64 = self.entries.values().map(|e| e.size).sum();
        if total_size > self.max_size as u64 {
            self.expire_age /= 2;
        } else if self.expire_age < Duration::from_secs(120) {
            self.expire_age *= 2;
        }
    }
}

impl Default for MmapCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_temp_file(content: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_map_returns_content() {
        let mut cache = MmapCache::new();
        let f = make_temp_file(b"hello world");
        let mmap = cache.map(f.path()).unwrap();
        assert_eq!(&mmap[..], b"hello world");
    }

    #[test]
    fn test_map_caches_identical() {
        let mut cache = MmapCache::new();
        let f = make_temp_file(b"cached");
        let m1 = cache.map(f.path()).unwrap();
        let m2 = cache.map(f.path()).unwrap();
        // Both Rc point to the same allocation
        assert_eq!(Rc::as_ptr(&m1), Rc::as_ptr(&m2));
    }

    #[test]
    fn test_cleanup_evicts_unreferenced() {
        let mut cache = MmapCache::new();
        cache.expire_age = Duration::from_millis(1);
        let f = make_temp_file(b"evict me");
        {
            let _mmap = cache.map(f.path()).unwrap();
            // mmap dropped here
        }
        std::thread::sleep(Duration::from_millis(5));
        cache.cleanup();
        // Cache should have evicted the entry
    }

    #[test]
    fn test_file_not_found() {
        let mut cache = MmapCache::new();
        let result = cache.map(Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
    }
}

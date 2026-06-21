use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;

static CACHE: OnceLock<Cache> = OnceLock::new();

pub fn init() -> &'static Cache {
    CACHE.get_or_init(Cache::new)
}

pub struct Cache {
    base: PathBuf,
}

impl Cache {
    fn new() -> Self {
        let base = if let Ok(dir) = std::env::var("XDG_CACHE_HOME") {
            PathBuf::from(dir)
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".cache")
        } else {
            PathBuf::from("/tmp")
        };
        let base = base.join("vigil");
        let _ = fs::create_dir_all(&base);
        Self { base }
    }

    fn path(&self, category: &str, key: &str) -> PathBuf {
        let safe_key = key.replace('/', "_");
        self.base.join(category).join(format!("{}.json", safe_key))
    }

    /// Get cached value if it exists and is not older than `max_age_hours`.
    pub fn get(&self, category: &str, key: &str, max_age_hours: u64) -> Option<String> {
        let path = self.path(category, key);
        let meta = fs::metadata(&path).ok()?;
        let modified = meta.modified().ok()?;
        let age = SystemTime::now().duration_since(modified).ok()?;
        if age.as_secs() > max_age_hours * 3600 {
            return None;
        }
        fs::read_to_string(&path).ok()
    }

    /// Store a value in the cache. Uses atomic write (write to .tmp, then rename).
    pub fn set(&self, category: &str, key: &str, data: &str) {
        let path = self.path(category, key);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

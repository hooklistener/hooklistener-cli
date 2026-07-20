use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub access_token: Option<String>,
    pub token_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub refresh_token_expires_at: Option<DateTime<Utc>>,
    pub selected_organization_id: Option<String>,
    #[serde(default)]
    pub last_update_check: Option<DateTime<Utc>>,
    #[serde(default)]
    pub latest_known_version: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        Self::load_from(&config_path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() {
            return Err(anyhow::anyhow!(
                "Config path must be a regular file and must not be a symlink"
            ));
        }

        #[cfg(unix)]
        let content = read_private_config_file(path)?;

        #[cfg(not(unix))]
        let content = fs::read_to_string(path)?;

        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;
        self.save_to(&config_path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        self.save_to_with_replace(path, |temp_path, path| {
            #[cfg(windows)]
            {
                replace_file_windows(temp_path, path)?;
            }

            #[cfg(not(windows))]
            fs::rename(temp_path, path)?;
            Ok(())
        })
    }

    fn save_to_with_replace<F>(&self, path: &Path, replace: F) -> Result<()>
    where
        F: FnOnce(&Path, &Path) -> Result<()>,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Config path must include a file name"))?;
        let temp_path = path.with_file_name(format!(
            ".{}.{}.tmp",
            file_name.to_string_lossy(),
            uuid::Uuid::new_v4()
        ));

        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;

            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)?
        };

        #[cfg(not(unix))]
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;

        let write_result = (|| -> Result<()> {
            let mut file = file;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            drop(file);
            replace(&temp_path, path)?;
            Ok(())
        })();

        if write_result.is_err() {
            #[cfg(windows)]
            remove_file_windows_best_effort(&temp_path);

            #[cfg(not(windows))]
            let _ = fs::remove_file(&temp_path);
        }

        write_result
    }

    pub fn config_path() -> Result<PathBuf> {
        let home =
            dirs::config_dir().ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;

        Ok(home.join("hooklistener").join("config.json"))
    }

    pub fn set_tokens(
        &mut self,
        access_token: String,
        expires_at: DateTime<Utc>,
        refresh_token: Option<String>,
        refresh_expires_at: Option<DateTime<Utc>>,
    ) {
        self.access_token = Some(access_token);
        self.token_expires_at = Some(expires_at);
        self.refresh_token = refresh_token;
        self.refresh_token_expires_at = refresh_expires_at;
    }

    pub fn is_token_valid(&self) -> bool {
        if let (Some(_), Some(expires_at)) = (&self.access_token, &self.token_expires_at) {
            Utc::now() < *expires_at
        } else {
            false
        }
    }

    pub fn is_refresh_token_valid(&self) -> bool {
        if let (Some(_), Some(expires_at)) = (&self.refresh_token, &self.refresh_token_expires_at) {
            Utc::now() < *expires_at
        } else {
            false
        }
    }

    pub fn clear_token(&mut self) {
        self.access_token = None;
        self.token_expires_at = None;
        self.refresh_token = None;
        self.refresh_token_expires_at = None;
    }

    #[cfg(test)]
    pub fn set_selected_organization(&mut self, organization_id: String) {
        self.selected_organization_id = Some(organization_id);
    }

    #[cfg(test)]
    pub fn clear_all(&mut self) {
        *self = Config::default();
    }
}

#[cfg(unix)]
fn read_private_config_file(path: &Path) -> Result<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mut file = File::open(path)?;
    let opened_metadata = file.metadata()?;
    let current_metadata = fs::symlink_metadata(path)?;
    if !current_metadata.file_type().is_file()
        || opened_metadata.dev() != current_metadata.dev()
        || opened_metadata.ino() != current_metadata.ino()
    {
        return Err(anyhow::anyhow!(
            "Config path changed while it was being opened"
        ));
    }

    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

#[cfg(windows)]
fn remove_file_windows_best_effort(path: &Path) {
    if let Ok(metadata) = fs::metadata(path)
        && metadata.permissions().readonly()
    {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
    let _ = fs::remove_file(path);
}

#[cfg(windows)]
fn replace_file_windows(temp_path: &Path, path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_UNABLE_TO_MOVE_REPLACEMENT_2, GetLastError};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let replacement = wide(temp_path);
    let destination = wide(path);
    if !path.try_exists()? {
        // SAFETY: both paths are valid, NUL-terminated UTF-16 buffers that remain alive
        // for the call. The paths share a directory, so this is an atomic rename.
        if unsafe {
            MoveFileExW(
                replacement.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        return Ok(());
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Config path must include a file name"))?;
    let backup_path = path.with_file_name(format!(
        ".{}.{}.backup",
        file_name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let backup = wide(&backup_path);

    // ReplaceFile preserves the destination's ACLs and attributes. A unique backup also
    // makes its documented partial-failure states recoverable.
    // SAFETY: all paths are valid, NUL-terminated UTF-16 buffers that remain alive for
    // the call; the reserved pointers are required to be null.
    let replaced = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if replaced != 0 {
        // The destination is committed once ReplaceFile succeeds. Backup cleanup must
        // not turn that successful commit into a reported save failure.
        remove_file_windows_best_effort(&backup_path);
        return Ok(());
    }

    // SAFETY: this must be called immediately after the failed Win32 operation.
    let error_code = unsafe { GetLastError() };
    let replace_error = std::io::Error::from_raw_os_error(error_code as i32);
    if error_code == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 {
        // In this documented state, the old destination has moved to the backup while
        // the new file remains at temp_path. Restore the old config's original name.
        // SAFETY: both paths are valid, NUL-terminated UTF-16 buffers that remain alive.
        if unsafe {
            MoveFileExW(
                backup.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(anyhow::anyhow!(
                "Could not replace config ({replace_error}); recovery also failed; old config remains at {}: {}",
                backup_path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    // For all other documented and generic errors, both files retain their names and no
    // backup exists. If an unexpected backup was created, leave it in place rather than
    // risk deleting the only complete copy.
    Err(replace_error.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;
    use tempfile::TempDir;

    fn config_path_in(dir: &TempDir) -> PathBuf {
        dir.path().join("config.json")
    }

    #[test]
    fn test_no_token_is_invalid() {
        let config = Config::default();
        assert!(!config.is_token_valid());
    }

    #[test]
    fn test_token_without_expiry_is_invalid() {
        let config = Config {
            access_token: Some("tok".to_string()),
            ..Config::default()
        };
        assert!(!config.is_token_valid());
    }

    #[test]
    fn test_expired_token_is_invalid() {
        let config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now() - Duration::hours(1)),
            ..Config::default()
        };
        assert!(!config.is_token_valid());
    }

    #[test]
    fn test_valid_token() {
        let config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now() + Duration::hours(1)),
            ..Config::default()
        };
        assert!(config.is_token_valid());
    }

    #[test]
    fn test_set_tokens() {
        let mut config = Config::default();
        let expires = Utc::now() + Duration::hours(1);
        let refresh_expires = Utc::now() + Duration::days(30);
        config.set_tokens(
            "my_token".to_string(),
            expires,
            Some("my_refresh".to_string()),
            Some(refresh_expires),
        );
        assert_eq!(config.access_token.as_deref(), Some("my_token"));
        assert_eq!(config.token_expires_at, Some(expires));
        assert_eq!(config.refresh_token.as_deref(), Some("my_refresh"));
        assert_eq!(config.refresh_token_expires_at, Some(refresh_expires));
    }

    #[test]
    fn test_clear_token_preserves_org() {
        let mut config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now()),
            refresh_token: Some("ref".to_string()),
            refresh_token_expires_at: Some(Utc::now()),
            selected_organization_id: Some("org-1".to_string()),
            ..Config::default()
        };
        config.clear_token();
        assert!(config.access_token.is_none());
        assert!(config.token_expires_at.is_none());
        assert!(config.refresh_token.is_none());
        assert!(config.refresh_token_expires_at.is_none());
        assert_eq!(config.selected_organization_id.as_deref(), Some("org-1"));
    }

    #[test]
    fn test_clear_all() {
        let mut config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now()),
            selected_organization_id: Some("org-1".to_string()),
            ..Config::default()
        };
        config.clear_all();
        assert!(config.access_token.is_none());
        assert!(config.token_expires_at.is_none());
        assert!(config.selected_organization_id.is_none());
    }

    #[test]
    fn test_set_selected_organization() {
        let mut config = Config::default();
        config.set_selected_organization("org-42".to_string());
        assert_eq!(config.selected_organization_id.as_deref(), Some("org-42"));
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);

        let expires = Utc::now() + Duration::hours(24);
        let config = Config {
            access_token: Some("roundtrip_token".to_string()),
            token_expires_at: Some(expires),
            selected_organization_id: Some("org-rt".to_string()),
            ..Config::default()
        };
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.access_token.as_deref(), Some("roundtrip_token"));
        assert_eq!(loaded.selected_organization_id.as_deref(), Some("org-rt"));
        assert!(loaded.is_token_valid());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn save_replaces_existing_config_without_leaving_temporary_files() {
        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);
        fs::write(&path, "old config contents").unwrap();

        let config = Config {
            access_token: Some("replacement_token".to_string()),
            token_expires_at: Some(Utc::now() + Duration::hours(1)),
            ..Config::default()
        };
        config.save_to(&path).unwrap();

        let entries = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(entries, 1);
        assert_eq!(
            Config::load_from(&path).unwrap().access_token.as_deref(),
            Some("replacement_token")
        );
    }

    #[test]
    fn failed_replace_preserves_old_config_and_removes_temporary_file() {
        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);
        let old_config = Config {
            access_token: Some("old_token".to_string()),
            ..Config::default()
        };
        old_config.save_to(&path).unwrap();

        let new_config = Config {
            access_token: Some("new_token".to_string()),
            ..Config::default()
        };
        let result = new_config.save_to_with_replace(&path, |_, _| {
            Err(anyhow::anyhow!("injected replacement failure"))
        });

        assert!(result.is_err());
        assert_eq!(
            Config::load_from(&path).unwrap().access_token.as_deref(),
            Some("old_token")
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn native_replace_failure_preserves_old_config_and_removes_temporary_file() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);
        let old_config = Config {
            access_token: Some("old_token".to_string()),
            ..Config::default()
        };
        old_config.save_to(&path).unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        let result = Config {
            access_token: Some("new_token".to_string()),
            ..Config::default()
        }
        .save_to(&path);

        assert!(result.is_err());
        drop(locked);
        assert_eq!(
            Config::load_from(&path).unwrap().access_token.as_deref(),
            Some("old_token")
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn native_replace_preserves_destination_attributes() {
        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);
        Config::default().save_to(&path).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        Config {
            access_token: Some("new_token".to_string()),
            ..Config::default()
        }
        .save_to(&path)
        .unwrap();

        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_sets_permissions_to_owner_read_write() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        Config::default().save_to(&path).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn concurrent_saves_only_expose_complete_json() {
        const WRITERS: usize = 12;

        let dir = TempDir::new().unwrap();
        let path = Arc::new(config_path_in(&dir));
        Config::default().save_to(&path).unwrap();
        let barrier = Arc::new(Barrier::new(WRITERS + 1));
        let writing = Arc::new(AtomicBool::new(true));

        let reader_path = Arc::clone(&path);
        let reader_barrier = Arc::clone(&barrier);
        let reader_writing = Arc::clone(&writing);
        let reader = thread::spawn(move || {
            reader_barrier.wait();
            while reader_writing.load(Ordering::Acquire) {
                let contents = fs::read_to_string(reader_path.as_ref()).unwrap();
                serde_json::from_str::<Config>(&contents).unwrap();
                thread::yield_now();
            }
        });

        let writers: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    Config {
                        access_token: Some(format!("writer-{writer}")),
                        latest_known_version: Some("x".repeat(32 * 1024)),
                        ..Config::default()
                    }
                    .save_to(path.as_ref())
                    .unwrap();
                })
            })
            .collect();

        for writer in writers {
            writer.join().unwrap();
        }
        writing.store(false, Ordering::Release);
        reader.join().unwrap();

        let final_contents = fs::read_to_string(path.as_ref()).unwrap();
        serde_json::from_str::<Config>(&final_contents).unwrap();
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");

        let config = Config::load_from(&path).unwrap();
        assert!(config.access_token.is_none());
        assert!(config.token_expires_at.is_none());
        assert!(config.selected_organization_id.is_none());
    }

    #[test]
    fn test_load_corrupted_json_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);

        fs::write(&path, "not valid json {{{").unwrap();

        let result = Config::load_from(&path);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn load_from_refuses_symlink_without_changing_target_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.json");
        fs::write(&target, "{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let path = config_path_in(&dir);
        symlink(&target, &path).unwrap();

        let result = Config::load_from(&path);

        assert!(result.is_err());
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}

use crate::database::{timestamp_ms, Database};
use crate::error::{AppError, Result};
#[cfg(unix)]
use cap_fs_ext::OpenOptionsSyncExt;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use uuid::Uuid;
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
static MUTATIONS: Mutex<()> = Mutex::new(());
#[derive(Clone)]
pub struct Workspace {
    pub id: String,
    pub root: PathBuf,
    directory: Arc<Dir>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub hash: String,
    pub encoding: String,
}
pub fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}
pub fn normalize_path(path: &str) -> Result<String> {
    if path.len() > 4096 || path.chars().any(|character| character.is_control()) {
        return Err(AppError::new(
            "PATH_INVALID",
            "The path contains unsupported characters or exceeds 4096 bytes",
        ));
    }
    let normalized = path.replace('\\', "/");
    if normalized.is_empty() {
        return Ok(normalized);
    }
    for part in normalized.split('/') {
        let lower = part.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or("");
        let device_suffix = stem
            .strip_prefix("com")
            .or_else(|| stem.strip_prefix("lpt"));
        let reserved = matches!(stem, "con" | "prn" | "aux" | "nul" | "conin$" | "conout$")
            || device_suffix.is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
        if part.is_empty()
            || matches!(part, "." | "..")
            || lower == ".git"
            || part.ends_with([' ', '.'])
            || part.contains([':', '*', '?', '"', '<', '>', '|', '~'])
            || reserved
        {
            return Err(AppError::new(
                "PATH_FORBIDDEN",
                "The path is not an allowed repository-relative path",
            ));
        }
    }
    Ok(normalized)
}
pub(crate) fn mutation_guard() -> Result<MutexGuard<'static, ()>> {
    MUTATIONS.lock().map_err(|_| {
        AppError::new(
            "FILESYSTEM_LOCK_POISONED",
            "The filesystem lock is unavailable; restart the application",
        )
    })
}
fn reject_link(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(AppError::new(
            "PATH_SYMLINK",
            "Symbolic links cannot be opened by the workspace",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(AppError::new(
                "PATH_REPARSE",
                "Windows reparse points cannot be opened by the workspace",
            ));
        }
    }
    Ok(())
}
fn validate_root(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if matches!(component, Component::Normal(_)) {
            reject_link(&current)?;
        }
    }
    Ok(())
}
impl Workspace {
    pub fn open(path: &str, database: &Database) -> Result<Self> {
        if path.is_empty() || path.chars().any(|character| character.is_control()) {
            return Err(AppError::new(
                "PATH_INVALID",
                "Choose a valid Git working directory",
            ));
        }
        let supplied = PathBuf::from(path);
        let supplied = if supplied.is_absolute() {
            supplied
        } else {
            std::env::current_dir()?.join(supplied)
        };
        validate_root(&supplied)?;
        let mut command = Command::new(crate::process::resolve_program("git", &supplied)?);
        command
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-C",
            ])
            .arg(&supplied)
            .args(["rev-parse", "--show-toplevel"]);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let output = command.output()?;
        if !output.status.success() {
            return Err(AppError::new(
                "NOT_GIT_REPOSITORY",
                "The selected directory is not a Git working tree",
            ));
        }
        let root_text = std::str::from_utf8(&output.stdout)
            .map_err(|_| AppError::new("PATH_ENCODING", "Git returned a path that is not UTF-8"))?
            .trim_end_matches(['\r', '\n']);
        let root = std::fs::canonicalize(root_text)?;
        validate_root(&root)?;
        let directory = Arc::new(Dir::open_ambient_dir(&root, cap_std::ambient_authority())?);
        let root_string = root.to_str().ok_or_else(|| {
            AppError::new("PATH_ENCODING", "Repository paths must be valid UTF-8")
        })?;
        let name = root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Repository");
        let id = database.with(|connection| {
            let prior: Option<String> = connection.query_row("SELECT id FROM repositories WHERE root=?1", [root_string], |row| row.get(0)).optional()?;
            let id = prior.unwrap_or_else(|| Uuid::new_v4().to_string());
            connection.execute("INSERT INTO repositories(id,root,name,opened_at) VALUES(?1,?2,?3,?4) ON CONFLICT(root) DO UPDATE SET opened_at=excluded.opened_at,name=excluded.name", params![id, root_string, name, timestamp_ms()])?;
            Ok(id)
        })?;
        Ok(Self {
            id,
            root,
            directory,
        })
    }
    fn parent(&self, path: &str) -> Result<(Dir, String)> {
        let normalized = normalize_path(path)?;
        if normalized.is_empty() {
            return Err(AppError::new(
                "PATH_ROOT",
                "This operation cannot modify the repository root",
            ));
        }
        let mut components = normalized.split('/').peekable();
        let mut directory = self.directory.try_clone()?;
        let mut relative = PathBuf::new();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                return Ok((directory, component.to_owned()));
            }
            relative.push(component);
            #[cfg(windows)]
            reject_link(&self.root.join(&relative))?;
            directory = directory.open_dir_nofollow(component)?;
        }
        Err(AppError::new("PATH_INVALID", "The path is invalid"))
    }
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        let normalized = normalize_path(path)?;
        if normalized.is_empty() {
            return Ok(self.root.clone());
        }
        let (directory, name) = self.parent(&normalized)?;
        match directory.symlink_metadata(&name) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(AppError::new(
                        "PATH_SYMLINK",
                        "Symbolic links are not accessible",
                    ));
                }
                #[cfg(windows)]
                reject_link(&self.root.join(&normalized))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(self.root.join(normalized))
    }
    pub fn tree(&self, path: &str) -> Result<Vec<FileEntry>> {
        let normalized = normalize_path(path)?;
        self.resolve(&normalized)?;
        let directory = if normalized.is_empty() {
            self.directory.try_clone()?
        } else {
            let (parent, name) = self.parent(&normalized)?;
            parent.open_dir_nofollow(name)?
        };
        let mut result = Vec::new();
        for entry in directory.entries()? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                AppError::new("PATH_ENCODING", "A directory entry is not valid UTF-8")
            })?;
            if name.eq_ignore_ascii_case(".git") || name.starts_with(".astraforge-write-") {
                continue;
            }
            let child = if normalized.is_empty() {
                name.clone()
            } else {
                format!("{normalized}/{name}")
            };
            if normalize_path(&child).is_err() {
                continue;
            }
            let metadata = directory.symlink_metadata(&name)?;
            if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
                continue;
            }
            #[cfg(windows)]
            if let Err(error) = reject_link(&self.root.join(&child)) {
                if error.code == "PATH_REPARSE" || error.code == "PATH_SYMLINK" {
                    continue;
                }
                return Err(error);
            }
            if result.len() == 20_000 {
                return Err(AppError::new(
                    "DIRECTORY_TOO_LARGE",
                    "This directory exceeds the 20,000-entry explorer limit; use indexed search",
                ));
            }
            result.push(FileEntry {
                path: child,
                name,
                kind: if metadata.is_dir() {
                    "directory"
                } else {
                    "file"
                }
                .into(),
                size: metadata.len(),
            });
        }
        result.sort_by(|left, right| {
            (left.kind != "directory", left.name.to_lowercase())
                .cmp(&(right.kind != "directory", right.name.to_lowercase()))
        });
        Ok(result)
    }
    pub fn read(&self, path: &str) -> Result<FileContent> {
        let normalized = normalize_path(path)?;
        self.resolve(&normalized)?;
        let (directory, name) = self.parent(&normalized)?;
        if !directory.symlink_metadata(&name)?.is_file() {
            return Err(AppError::new(
                "NOT_TEXT_FILE",
                "The requested path is not a regular text file",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        options.nonblock(true);
        let file = directory.open_with(&name, &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(AppError::new(
                "NOT_TEXT_FILE",
                "The requested path is not a regular text file",
            ));
        }
        if metadata.len() > MAX_TEXT_BYTES as u64 {
            return Err(AppError::new(
                "FILE_TOO_LARGE",
                "Text files must be at most 2 MiB",
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take((MAX_TEXT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_TEXT_BYTES {
            return Err(AppError::new(
                "FILE_TOO_LARGE",
                "Text files must be at most 2 MiB",
            ));
        }
        if bytes.contains(&0) {
            return Err(AppError::new(
                "BINARY_FILE",
                "Binary and UTF-16 files cannot be edited as UTF-8",
            ));
        }
        let content = String::from_utf8(bytes).map_err(|_| {
            AppError::new(
                "UNSUPPORTED_ENCODING",
                "Only UTF-8 text files can be edited",
            )
        })?;
        let encoding = if content.starts_with('\u{feff}') {
            "utf-8-bom"
        } else {
            "utf-8"
        }
        .into();
        Ok(FileContent {
            path: normalized,
            hash: content_hash(&content),
            content,
            encoding,
        })
    }
    pub fn write(&self, path: &str, content: &str, expected_hash: &str) -> Result<FileContent> {
        let _guard = mutation_guard()?;
        self.replace(path, Some(content), Some(expected_hash))?;
        self.read(path)
    }
    pub fn create(&self, path: &str, directory: bool) -> Result<()> {
        let _guard = mutation_guard()?;
        self.resolve(path)?;
        if directory {
            let (parent, name) = self.parent(path)?;
            parent.create_dir(name)?;
            Self::sync_directory(&parent)?;
            Ok(())
        } else {
            self.replace(path, Some(""), None)
        }
    }
    pub fn remove(&self, path: &str, expected_hash: Option<&str>) -> Result<()> {
        let _guard = mutation_guard()?;
        self.resolve(path)?;
        let (directory, name) = self.parent(path)?;
        let metadata = directory.symlink_metadata(&name)?;
        if metadata.is_dir() {
            directory.remove_dir(&name)?;
            Self::sync_directory(&directory)?;
            Ok(())
        } else {
            let hash = expected_hash.ok_or_else(|| {
                AppError::new(
                    "HASH_REQUIRED",
                    "Deleting a file requires its current content hash",
                )
            })?;
            self.replace(path, None, Some(hash))
        }
    }
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let _guard = mutation_guard()?;
        self.resolve(from)?;
        self.resolve(to)?;
        let (source, source_name) = self.parent(from)?;
        let (target, target_name) = self.parent(to)?;
        match target.symlink_metadata(&target_name) {
            Ok(_) => {
                return Err(AppError::new(
                    "ALREADY_EXISTS",
                    "The destination already exists",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        source.rename(&source_name, &target, &target_name)?;
        Self::sync_directory(&source)?;
        Self::sync_directory(&target)?;
        Ok(())
    }
    pub(crate) fn snapshot(&self, path: &str) -> Result<Option<FileContent>> {
        match self.read(path) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.code == "NOT_FOUND" => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub(crate) fn replace(
        &self,
        path: &str,
        content: Option<&str>,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        let normalized = normalize_path(path)?;
        self.resolve(&normalized)?;
        if let Some(content) = content {
            if content.len() > MAX_TEXT_BYTES || content.contains('\0') {
                return Err(AppError::new(
                    "INVALID_TEXT",
                    "Replacement content must be UTF-8 text no larger than 2 MiB without NUL bytes",
                ));
            }
        }
        let current = self.snapshot(&normalized)?;
        if current.as_ref().map(|file| file.hash.as_str()) != expected_hash {
            return Err(AppError::new("STALE_FILE", "The file changed; reload it before applying this operation")
                .context(serde_json::json!({"path": normalized, "expectedHash": expected_hash, "actualHash": current.as_ref().map(|file| &file.hash)})));
        }
        let (directory, name) = self.parent(&normalized)?;
        if let Some(content) = content {
            let permissions = if current.is_some() {
                Some(directory.metadata(&name)?.permissions())
            } else {
                None
            };
            if permissions
                .as_ref()
                .is_some_and(|permissions| permissions.readonly())
            {
                return Err(AppError::new("PERMISSION_DENIED", "The file is read-only"));
            }
            let temporary = format!(".astraforge-write-{}.tmp", Uuid::new_v4());
            let operation = (|| -> Result<()> {
                let mut options = OpenOptions::new();
                options
                    .write(true)
                    .create_new(true)
                    .follow(FollowSymlinks::No);
                let mut file = directory.open_with(&temporary, &options)?;
                file.write_all(content.as_bytes())?;
                if let Some(permissions) = permissions {
                    file.set_permissions(permissions)?;
                }
                file.sync_all()?;
                drop(file);
                let latest = self.snapshot(&normalized)?;
                if latest.as_ref().map(|file| file.hash.as_str()) != expected_hash {
                    return Err(AppError::new(
                        "STALE_FILE",
                        "The file changed while preparing the write",
                    )
                    .context(serde_json::json!({"path": normalized})));
                }
                if expected_hash.is_none() {
                    directory.hard_link(&temporary, &directory, &name)?;
                    directory.remove_file(&temporary)?;
                } else {
                    directory.rename(&temporary, &directory, &name)?;
                }
                Self::sync_directory(&directory)?;
                Ok(())
            })();
            if operation.is_err() {
                match directory.remove_file(&temporary) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(AppError::new("WRITE_CLEANUP_FAILED", "A failed write left a temporary file; inspect the repository").context(serde_json::json!({"path": normalized, "temporary": temporary, "cleanup": AppError::from(error), "operation": operation.err()}))),
                }
            }
            operation
        } else {
            if current.is_none() {
                return Err(AppError::new(
                    "NOT_FOUND",
                    "The file to remove does not exist",
                ));
            }
            directory.remove_file(&name)?;
            Self::sync_directory(&directory)?;
            Ok(())
        }
    }
    fn sync_directory(directory: &Dir) -> Result<()> {
        #[cfg(unix)]
        directory.try_clone()?.into_std_file().sync_all()?;
        #[cfg(not(unix))]
        let _ = directory;
        Ok(())
    }
}

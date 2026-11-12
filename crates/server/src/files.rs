//! `put_file` and `get_file`: pictures between the phone and the computer.
//! A picture from the phone is saved under the system's temp folder, where
//! the Codex app keeps pasted ones too, and the agent opens it by path.

use crate::{Fault, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::path::{Path, PathBuf};
use tether_proto::{ErrorCode, FileContent, FileData, MAX_FILE};

/// Where pictures from the phone go.
pub(crate) fn uploads() -> PathBuf {
    std::env::temp_dir().join("tether-uploads")
}

/// The picture types a conversation shows, by extension.
fn mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        _ => return None,
    })
}

fn bad(message: impl Into<String>) -> Fault {
    Fault::new(ErrorCode::BadRequest, message)
}

/// Saves `data` (base64) in `dir` under a new name that ends like `name`.
pub(crate) async fn put(dir: &Path, name: &str, data: &str) -> Result<FileData> {
    // A name from the phone is only a hint: no folders, nothing odd.
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let clean: String = base
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let file = format!("{}-{}", uuid::Uuid::new_v4().simple(), clean);
    let path = dir.join(&file);
    if mime(&path).is_none() {
        return Err(bad("only pictures can be sent"));
    }
    if data.len() > MAX_FILE.div_ceil(3) * 4 {
        return Err(bad("picture too large"));
    }
    let bytes = STANDARD
        .decode(data.trim())
        .map_err(|_| bad("data is not base64"))?;
    if bytes.len() > MAX_FILE {
        return Err(bad("picture too large"));
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(anyhow::Error::from)?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(FileData {
        path: path.display().to_string(),
    })
}

/// A picture on the computer, by its full path.
pub(crate) async fn get(path: &str) -> Result<FileContent> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(bad("path must be absolute"));
    }
    let mime = mime(&path).ok_or_else(|| bad("not a picture"))?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| Fault::new(ErrorCode::NotFound, "picture not found"))?;
    if !meta.is_file() {
        return Err(Fault::new(ErrorCode::NotFound, "picture not found"));
    }
    if meta.len() > MAX_FILE as u64 {
        return Err(bad("picture too large"));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| Fault::new(ErrorCode::NotFound, "picture not found"))?;
    Ok(FileContent {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        mime: mime.into(),
        data: STANDARD.encode(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_picture_put_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let saved = put(dir.path(), "../截图 1.PNG", &STANDARD.encode(b"png"))
            .await
            .unwrap();
        let path = PathBuf::from(&saved.path);
        assert_eq!(path.parent(), Some(dir.path()));
        assert!(saved.path.ends_with("-截图1.PNG"), "{}", saved.path);
        let back = get(&saved.path).await.unwrap();
        assert_eq!(back.mime, "image/png");
        assert_eq!(STANDARD.decode(back.data).unwrap(), b"png");
    }

    #[tokio::test]
    async fn only_pictures_and_only_small_ones() {
        let dir = tempfile::tempdir().unwrap();
        assert!(put(dir.path(), "a.exe", "eA==").await.is_err());
        assert!(put(dir.path(), "a.png", "not base64!").await.is_err());
        let big = "A".repeat(MAX_FILE / 3 * 4 + 8);
        assert!(put(dir.path(), "a.png", &big).await.is_err());

        let text = dir.path().join("a.txt");
        std::fs::write(&text, "x").unwrap();
        assert!(get(&text.display().to_string()).await.is_err());
        assert!(get("a.png").await.is_err());
        let missing = dir.path().join("none.png");
        assert!(get(&missing.display().to_string()).await.is_err());
    }
}

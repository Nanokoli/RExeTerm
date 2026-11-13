//! File data adapter. Listeners only establish Links; no HTTP side route exists.
use crate::{
    data::{
        file::{receive_content, send_content, send_message},
        staged_file::StagedFile,
    },
    protocol::file::{FileMessage, FileRequest, FileTransferResult},
    tools::fs::file_stamp,
    transport::Link,
};
use anyhow::{bail, Result};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{io::AsyncSeekExt, sync::Mutex};

pub(crate) async fn serve_file_transfer(
    mut link: Link,
    request: FileRequest,
    write_lock: Arc<Mutex<()>>,
    mut activity: crate::events::EventSpan,
) -> Result<()> {
    let result = async {
        match request {
            FileRequest::Download {
                path,
                directory,
                offset,
            } => download(&mut link, resolve(directory, &path)?, offset).await,
            FileRequest::Upload {
                path,
                directory,
                size,
                overwrite,
                sha256,
            } => {
                let _guard =
                    tokio::time::timeout(std::time::Duration::from_secs(30), write_lock.lock())
                        .await
                        .map_err(|_| anyhow::anyhow!("file write lock timed out"))?;
                upload(
                    &mut link,
                    resolve(directory, &path)?,
                    size,
                    overwrite,
                    sha256,
                )
                .await
            }
        }
    }
    .await;
    match &result {
        Ok(bytes) => activity.set("bytes", serde_json::json!(bytes)),
        Err(error) => activity.set("error", serde_json::json!(error.to_string())),
    }
    activity.finish(if result.is_ok() { "ok" } else { "error" });
    if let Err(error) = result {
        send_message(
            &mut link,
            &FileMessage::Error {
                message: error.to_string(),
            },
        )
        .await?;
    }
    Ok(())
}
fn resolve(directory: Option<PathBuf>, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("file path is required")
    }
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        directory.unwrap_or(std::env::current_dir()?).join(path)
    })
}
async fn download(link: &mut Link, path: PathBuf, offset: u64) -> Result<u64> {
    let mut file = tokio::fs::File::open(&path).await?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() {
        bail!("download only supports files")
    }
    if offset > metadata.len() {
        bail!("offset exceeds file size")
    }
    let bytes = metadata.len() - offset;
    let stamp = serde_json::to_value(file_stamp(&path)?)?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    send_message(link, &FileMessage::Ready { bytes, offset }).await?;
    let digest = send_content(link, &mut file, bytes, None).await?;
    send_message(
        link,
        &FileMessage::Done {
            result: FileTransferResult {
                bytes: digest.bytes,
                sha256: digest.sha256,
                file: stamp,
            },
        },
    )
    .await?;
    Ok(digest.bytes)
}
async fn upload(
    link: &mut Link,
    path: PathBuf,
    size: u64,
    overwrite: bool,
    sha256: Option<String>,
) -> Result<u64> {
    let (staging, mut file) = StagedFile::create(&path, overwrite)?;
    send_message(
        link,
        &FileMessage::Ready {
            bytes: size,
            offset: 0,
        },
    )
    .await?;
    let digest = receive_content(link, &mut file, size, None).await?;
    if sha256.is_some_and(|expected| expected != digest.sha256) {
        bail!("upload sha256 does not match expected digest")
    }
    staging.commit(file).await?;
    let stamp = serde_json::to_value(file_stamp(&path)?)?;
    send_message(
        link,
        &FileMessage::Done {
            result: FileTransferResult {
                bytes: digest.bytes,
                sha256: digest.sha256,
                file: stamp,
            },
        },
    )
    .await?;
    Ok(digest.bytes)
}

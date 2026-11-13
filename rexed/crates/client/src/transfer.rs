//! File operations use endpoint discovery, channel limits and the injected Connector.
use super::{
    endpoint::ExecutorEndpoint,
    options::{FileTransferMode, FileTransferOptions},
};
use crate::{
    data::{
        file::{self, TransferObserver},
        staged_file::StagedFile,
    },
    protocol::file::{FileMessage, FileRequest, FileTransferResult},
    transport::{ChannelLease, Link, Purpose},
};
use anyhow::{anyhow, bail, Result};
use std::path::Path;
use tokio::time::{timeout, Duration};
impl ExecutorEndpoint {
    pub(super) async fn transfer_file(
        &self,
        options: FileTransferOptions,
        directory: Option<&Path>,
        observer: Option<&dyn TransferObserver>,
    ) -> Result<FileTransferResult> {
        options.metadata.validate().map_err(anyhow::Error::msg)?;
        let pool = self.file_pool()?;
        let (mut link, lease) = timeout(Duration::from_secs(30), pool.connect(Purpose::File))
            .await
            .map_err(|_| anyhow!("file channel open timed out"))??;
        if let Some(session) = &self.session {
            session
                .bind_named(
                    &mut link,
                    if options.metadata.title != "未命名"
                        || !options.metadata.description.is_empty()
                    {
                        Some(options.metadata.clone())
                    } else {
                        None
                    },
                )
                .await?;
        }
        let result = transfer(&mut link, options, directory, observer).await;
        retire(link, lease);
        result
    }
}
async fn transfer(
    link: &mut Link,
    options: FileTransferOptions,
    directory: Option<&Path>,
    observer: Option<&dyn TransferObserver>,
) -> Result<FileTransferResult> {
    let directory = directory.map(Path::to_owned);
    let (sent, staging) = match options.mode {
        FileTransferMode::Download => {
            let (stage, mut local) =
                StagedFile::create(&options.local_path, options.overwrite.unwrap_or(false))?;
            file::send_message(
                link,
                &FileMessage::Open {
                    request: FileRequest::Download {
                        path: options.target_path,
                        directory,
                        offset: options.offset,
                    },
                },
            )
            .await?;
            let FileMessage::Ready { bytes, offset } = file::receive_message(link).await? else {
                bail!("expected file ready")
            };
            if offset != options.offset {
                bail!("unexpected download offset")
            }
            let digest = file::receive_content(link, &mut local, bytes, observer).await?;
            (digest, Some((stage, local)))
        }
        FileTransferMode::Upload => {
            if options.offset != 0 {
                bail!("upload offset is not supported")
            }
            let mut local = tokio::fs::File::open(&options.local_path).await?;
            let metadata = local.metadata().await?;
            if !metadata.is_file() {
                bail!("upload only supports files")
            }
            let bytes = metadata.len();
            file::send_message(
                link,
                &FileMessage::Open {
                    request: FileRequest::Upload {
                        path: options.target_path,
                        directory,
                        size: bytes,
                        overwrite: options.overwrite.unwrap_or(false),
                        sha256: options.sha256,
                    },
                },
            )
            .await?;
            let FileMessage::Ready {
                bytes: expected,
                offset: 0,
            } = file::receive_message(link).await?
            else {
                bail!("expected file ready")
            };
            if expected != bytes {
                bail!("unexpected upload size")
            }
            (
                file::send_content(link, &mut local, bytes, observer).await?,
                None,
            )
        }
    };
    let FileMessage::Done { result } = file::receive_message(link).await? else {
        bail!("expected file completion")
    };
    if result.bytes != sent.bytes || result.sha256 != sent.sha256 {
        bail!("file completion digest mismatch")
    }
    if let Some((stage, local)) = staging {
        stage.commit(local).await?;
    }
    Ok(result)
}
fn retire(mut link: Link, lease: ChannelLease) {
    tokio::spawn(async move {
        let _lease = lease;
        let _ = timeout(Duration::from_secs(1), link.close()).await;
    });
}

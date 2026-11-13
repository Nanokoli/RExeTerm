//! Cancellation-safe staging shared by local downloads and remote uploads.
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
pub struct StagedFile {
    temp: tempfile::NamedTempFile,
    target: PathBuf,
    overwrite: bool,
}
impl StagedFile {
    pub fn create(target: &Path, overwrite: bool) -> Result<(Self, tokio::fs::File)> {
        if target.exists() && !overwrite {
            bail!(
                "target already exists; pass overwrite=true: {}",
                target.display()
            )
        }
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temp = tempfile::Builder::new()
            .prefix(".re-transfer-")
            .suffix(".tmp")
            .tempfile_in(parent)?;
        let file = tokio::fs::File::from_std(temp.reopen()?);
        Ok((
            Self {
                temp,
                target: target.to_owned(),
                overwrite,
            },
            file,
        ))
    }
    /// Keep the original target until the complete, verified file replaces it.
    pub async fn commit(self, file: tokio::fs::File) -> Result<()> {
        file.sync_all().await?;
        drop(file);
        tokio::task::spawn_blocking(move || {
            let result = if self.overwrite {
                self.temp.persist(&self.target)
            } else {
                self.temp.persist_noclobber(&self.target)
            };
            result
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!(e))
                .with_context(|| format!("failed to commit {}", self.target.display()))
        })
        .await??;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    #[tokio::test]
    async fn no_clobber_preserves_a_target_created_after_staging() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let (stage, mut file) = StagedFile::create(&target, false).unwrap();
        file.write_all(b"staged").await.unwrap();
        std::fs::write(&target, b"concurrent writer").unwrap();
        assert!(stage.commit(file).await.is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"concurrent writer");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

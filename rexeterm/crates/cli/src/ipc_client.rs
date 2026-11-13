//! Single-result frontend for a daemon on this machine; owns no RE connection.
use anyhow::{ensure, Context, Result};
use rexeterm_cli_host::ipc::{self, Request, Response};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
pub struct IpcClient {
    pub target: String,
    token: String,
    service_id: String,
}
impl IpcClient {
    pub fn new(target: String, token: String, service_id: &str) -> Result<Self> {
        let service_id = rexeterm_cli_host::normalize_id(service_id)?;
        ensure!(
            token.len() == 64 && token.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid IPC token"
        );
        Ok(Self {
            target,
            token,
            service_id,
        })
    }
    pub async fn call(&self, operation: &str, args: Value) -> Result<Value> {
        self.call_with_key(operation, args, None).await
    }
    pub async fn call_with_key(
        &self,
        operation: &str,
        args: Value,
        key: Option<String>,
    ) -> Result<Value> {
        let request = Request {
            service_id: self.service_id.clone(),
            token: self.token.clone(),
            operation: operation.into(),
            args,
            key,
        };
        let mut io = ipc::connect(&self.target)
            .await
            .context("bound CLI host is unavailable; run service start or select another ID")?;
        ipc::write(&mut io, &request, ipc::MAX_REQUEST).await?;
        let reply: Response = ipc::read(&mut io, ipc::MAX_RESPONSE).await?;
        ensure!(
            reply.service_id == self.service_id,
            "CLI host service ID mismatch"
        );
        if let Some(error) = reply.error {
            anyhow::bail!("{error}");
        }
        reply
            .result
            .ok_or_else(|| anyhow::anyhow!("missing IPC result"))
    }
    pub async fn verify_service(&self) -> Result<()> {
        let info = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.call("info", json!({})),
        )
        .await??;
        ensure!(
            info["serviceId"].as_str() == Some(&self.service_id),
            "CLI host service ID mismatch"
        );
        Ok(())
    }
    pub async fn wait_transfer(&self, id: &str) -> Result<Value> {
        loop {
            let view = self.call("transfers.show", json!({"id":id})).await?;
            match view["status"].as_str() {
                Some("done") => return Ok(view),
                Some("error" | "cancelled") => anyhow::bail!("transfer {id} failed: {view}"),
                _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    }
    pub async fn download(
        &self,
        host: &str,
        mut body: Value,
        output: PathBuf,
        overwrite: bool,
    ) -> Result<Value> {
        ensure!(
            overwrite || !output.exists(),
            "output already exists: {}",
            output.display()
        );
        body["localPath"] = json!(std::path::absolute(output)?);
        body["overwrite"] = json!(overwrite);
        let job = self
            .call("transfers.local", json!({"host":host,"options":body}))
            .await?;
        self.wait_transfer(
            job["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing transfer ID"))?,
        )
        .await
    }
    pub async fn upload(&self, host: &str, input: &Path, mut body: Value) -> Result<Value> {
        body["localPath"] = json!(std::fs::canonicalize(input)?);
        let job = self
            .call("transfers.local", json!({"host":host,"options":body}))
            .await?;
        self.wait_transfer(
            job["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing transfer ID"))?,
        )
        .await
    }
    pub async fn save_content(&self, id: &str, output: PathBuf, overwrite: bool) -> Result<Value> {
        let result = self.call("transfers.content", json!({"id":id})).await?;
        let source = result["localPath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing staged content path"))?;
        ensure!(
            overwrite || !output.exists(),
            "output already exists: {}",
            output.display()
        );
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let (file, path) = tempfile::NamedTempFile::new_in(parent)?.into_parts();
        let mut file = tokio::fs::File::from_std(file);
        let mut input = tokio::fs::File::open(source).await?;
        let bytes = tokio::io::copy(&mut input, &mut file).await?;
        file.sync_all().await?;
        drop(file);
        ensure!(
            result["bytes"].as_u64() == Some(bytes),
            "staged content size changed"
        );
        if overwrite {
            path.persist(&output)?;
        } else {
            path.persist_noclobber(&output)?;
        }
        Ok(json!({"output":output,"bytes":bytes,"saved":true}))
    }
}

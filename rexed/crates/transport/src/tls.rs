//! TLS is a physical-link concern; application authentication stays above framing.
use crate::{Connector, DefaultConnector, Link, LinkFuture};
use anyhow::{anyhow, bail, Context, Result};
pub use rustls;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde::Deserialize;
use std::{
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TlsClientOptions {
    /// Additional trust anchor PEM; hostname verification is always enabled.
    pub ca_file: Option<PathBuf>,
    pub certificate_file: Option<PathBuf>,
    pub private_key_file: Option<PathBuf>,
    /// Verify this DNS name when dialing an IP address or a tunnel.
    pub server_name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LinkSecurity {
    pub encrypted: bool,
    /// Presented peer chain with private-key possession verified by TLS.
    /// The configured verifier defines trust; RE servers enforce per-user pin/CA trust at login.
    pub peer_certificates: Vec<Vec<u8>>,
}

pub fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("read certificate {}", path.display()))?;
    let certs =
        rustls_pemfile::certs(&mut BufReader::new(file)).collect::<std::io::Result<Vec<_>>>()?;
    anyhow::ensure!(!certs.is_empty(), "certificate PEM is empty");
    Ok(certs)
}
pub fn private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut BufReader::new(std::fs::File::open(path)?))?
        .ok_or_else(|| anyhow!("private key PEM is empty"))
}
pub fn roots(paths: &[PathBuf]) -> Result<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    for path in paths {
        for cert in certificates(path)? {
            roots.add(cert)?;
        }
    }
    Ok(roots)
}

pub struct TlsConnector {
    options: TlsClientOptions,
    config: Arc<rustls::ClientConfig>,
}
impl TlsConnector {
    pub fn new(options: TlsClientOptions) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(path) = &options.ca_file {
            for cert in certificates(path)? {
                roots.add(cert)?;
            }
        }
        let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots);
        let config = match (&options.certificate_file, &options.private_key_file) {
            (Some(cert), Some(key)) => {
                builder.with_client_auth_cert(certificates(cert)?, private_key(key)?)?
            }
            (None, None) => builder.with_no_client_auth(),
            _ => bail!("client certificate and private key must be configured together"),
        };
        Ok(Self {
            options,
            config: Arc::new(config),
        })
    }
}
impl Connector for TlsConnector {
    fn max_parallel_links(&self, _: &str) -> usize {
        32
    }
    fn connect<'a>(&'a self, target: &'a str) -> LinkFuture<'a, Link> {
        Box::pin(async move {
            if !target.starts_with("tls://") && !target.starts_with("wss://") {
                return DefaultConnector.connect(target).await;
            }
            let url = url::Url::parse(target)?;
            anyhow::ensure!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "credentials, query and fragment are not allowed in a link URL"
            );
            let host = url
                .host_str()
                .ok_or_else(|| anyhow!("TLS host missing"))?
                .trim_matches(['[', ']']);
            let port = url
                .port()
                .or_else(|| (url.scheme() == "wss").then_some(443))
                .ok_or_else(|| anyhow!("TLS port missing"))?;
            anyhow::ensure!(
                url.scheme() != "tls" || url.path().is_empty() || url.path() == "/",
                "TLS stream target cannot contain a path"
            );
            let name = ServerName::try_from(
                self.options
                    .server_name
                    .as_deref()
                    .unwrap_or(host)
                    .to_owned(),
            )?;
            let stream = TcpStream::connect((host, port)).await?;
            stream.set_nodelay(true)?;
            let meter = Arc::new(crate::TrafficMeter::default());
            let stream = crate::CountedIo::new(stream, meter.clone());
            let tls = tokio_rustls::TlsConnector::from(self.config.clone())
                .connect(name, stream)
                .await?;
            let security = LinkSecurity {
                encrypted: true,
                peer_certificates: tls
                    .get_ref()
                    .1
                    .peer_certificates()
                    .unwrap_or_default()
                    .iter()
                    .map(|c| c.as_ref().to_vec())
                    .collect(),
            };
            let link = if url.scheme() == "wss" {
                let (ws, _) = tokio_tungstenite::client_async(target, tls).await?;
                crate::from_websocket(ws)
            } else {
                crate::from_stream(tls)
            };
            Ok(link.with_traffic(meter).with_security(security))
        })
    }
}

/// Accept standard TLS, then choose opaque REX1 or WebSocket framing.
pub async fn accept_tls<S>(
    stream: S,
    config: Arc<rustls::ServerConfig>,
    websocket_path: Option<String>,
) -> Result<Link>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let meter = Arc::new(crate::TrafficMeter::default());
    let stream = crate::CountedIo::new(stream, meter.clone());
    let tls = tokio_rustls::TlsAcceptor::from(config)
        .accept(stream)
        .await?;
    let security = LinkSecurity {
        encrypted: true,
        peer_certificates: tls
            .get_ref()
            .1
            .peer_certificates()
            .unwrap_or_default()
            .iter()
            .map(|c| c.as_ref().to_vec())
            .collect(),
    };
    let link = match websocket_path {
        Some(path) => crate::accept_websocket_at(tls, path).await?,
        None => crate::from_stream(tls),
    };
    Ok(link.with_traffic(meter).with_security(security))
}

//! A broken listening socket must not permanently stop an otherwise live server.
use crate::events::EventLog;
use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
    time::Duration,
};
use tokio::net::{TcpListener, TcpSocket, TcpStream};

pub(crate) fn bind_tcp(address: &str) -> io::Result<TcpListener> {
    let mut last = io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no listening address resolved",
    );
    for address in address.to_socket_addrs()? {
        let result = (|| {
            let socket = if address.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            // Unix requires this on the original listener to rebind while accepted streams exist.
            #[cfg(unix)]
            socket.set_reuseaddr(true)?;
            socket.bind(address)?;
            socket.listen(1024)
        })();
        match result {
            Ok(listener) => return Ok(listener),
            Err(error) => last = error,
        }
    }
    Err(last)
}

pub(crate) struct ResilientTcpListener {
    listener: Option<TcpListener>,
    address: SocketAddr,
    events: EventLog,
    delay: Duration,
}
impl ResilientTcpListener {
    pub(crate) fn new(listener: TcpListener, events: EventLog) -> io::Result<Self> {
        Ok(Self {
            address: listener.local_addr()?,
            listener: Some(listener),
            events,
            delay: Duration::from_millis(100),
        })
    }
    pub(crate) async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        loop {
            if self.listener.is_none() {
                match bind_tcp(&self.address.to_string()) {
                    Ok(listener) => {
                        self.listener = Some(listener);
                        self.events.emit(
                            "listener.rebound",
                            None,
                            serde_json::json!({"address":self.address.to_string()}),
                        );
                    }
                    Err(error) => {
                        self.failed("bind", &error).await;
                        continue;
                    }
                }
            }
            match self.listener.as_ref().unwrap().accept().await {
                Ok(stream) => {
                    self.delay = Duration::from_millis(100);
                    return stream;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    // Accepted connections own their sockets and survive replacing only the listener.
                    self.listener.take();
                    self.failed("accept", &error).await;
                }
            }
        }
    }
    async fn failed(&mut self, operation: &str, error: &io::Error) {
        self.events.emit("listener.retry", None, serde_json::json!({"address":self.address.to_string(),"operation":operation,"error":error.to_string(),"retryMs":self.delay.as_millis()}));
        eprintln!(
            "rexed listener {} {operation} failed: {error}; retry in {} ms",
            self.address,
            self.delay.as_millis()
        );
        tokio::time::sleep(self.delay).await;
        self.delay = (self.delay * 2).min(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::timeout,
    };
    #[cfg(unix)]
    #[tokio::test]
    async fn real_accept_failure_rebinds_same_port_and_preserves_existing_stream() {
        use std::os::fd::AsRawFd;
        let listener = bind_tcp("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut listener = ResilientTcpListener::new(listener, EventLog::default()).unwrap();
        let mut existing = TcpStream::connect(address).await.unwrap();
        let (mut accepted, _) = listener.accept().await;
        // Force the OS listener into a failed accept state without changing the interface or IP.
        let fd = listener.listener.as_ref().unwrap().as_raw_fd();
        // SAFETY: fd is a live, borrowed listening socket; shutdown does not close/reuse its fd.
        assert_eq!(unsafe { libc::shutdown(fd, libc::SHUT_RDWR) }, 0);
        let next = tokio::spawn(async move { listener.accept().await });
        let mut connected = timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = TcpStream::connect(address).await {
                    break stream;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let (mut new_accepted, _) = timeout(Duration::from_secs(5), next)
            .await
            .unwrap()
            .unwrap();
        existing.write_all(b"old").await.unwrap();
        let mut bytes = [0; 3];
        accepted.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"old");
        connected.write_all(b"new").await.unwrap();
        new_accepted.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"new");
    }
}

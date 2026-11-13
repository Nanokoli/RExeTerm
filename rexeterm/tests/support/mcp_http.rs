use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
pub async fn http(
    address: SocketAddr,
    method: &str,
    session: Option<&str>,
    extra: &str,
    body: &str,
) -> (u16, String, Value) {
    let mut socket = TcpStream::connect(address).await.unwrap();
    let session = session
        .map(|s| format!("Mcp-Session-Id: {s}\r\nMCP-Protocol-Version: 2025-06-18\r\n"))
        .unwrap_or_default();
    let request = format!("{method} /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nConnection: close\r\n{session}{extra}Content-Length: {}\r\n\r\n{body}", body.len());
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(response).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    (
        status,
        headers.to_owned(),
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
}
pub async fn init(address: SocketAddr) -> String {
    let (code, headers, response) = http(address, "POST", None, "", &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}).to_string()).await;
    assert_eq!(code, 200, "{headers}");
    assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
    headers
        .lines()
        .find_map(|s| s.strip_prefix("mcp-session-id: "))
        .unwrap()
        .to_owned()
}
pub async fn tool(address: SocketAddr, session: &str, name: &str, arguments: Value) -> Value {
    let (code, _, value) = http(address, "POST", Some(session), "", &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string()).await;
    assert_eq!(code, 200);
    assert_eq!(value["result"]["isError"], false, "{value}");
    value["result"]["structuredContent"].clone()
}

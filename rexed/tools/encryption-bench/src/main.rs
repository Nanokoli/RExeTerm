use anyhow::{anyhow, ensure, Result};
use rexe_client::{
    protocol::{terminal::ServerText, ExecutorRequest},
    transport::Frame,
    Client, FileTransferOptions,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const PTY_BYTES: usize = 131_072_000;
const BULK_BYTES: usize = 256 * 1024 * 1024;
fn cpu(pid: &str) -> f64 {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = s.rsplit_once(')').unwrap().1.split_whitespace().collect();
    (fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap()) / 100.0
}
struct Sample {
    start: Instant,
    server: f64,
    client: f64,
    pid: String,
}
impl Sample {
    fn new(pid: &str) -> Self {
        Self {
            start: Instant::now(),
            server: cpu(pid),
            client: cpu("self"),
            pid: pid.into(),
        }
    }
    fn result(self, kind: &str, bytes: usize) -> Value {
        let seconds = self.start.elapsed().as_secs_f64();
        json!({"kind":kind,"bytes":bytes,"seconds":seconds,"MBps":bytes as f64/seconds/1e6,"serverCpuSeconds":cpu(&self.pid)-self.server,"clientCpuSeconds":cpu("self")-self.client,"complete":true})
    }
}
async fn rpc(client: &Client, tool: &str, params: Value) -> Result<Value> {
    let response = client
        .handle(ExecutorRequest {
            id: json!(1),
            method: tool.into(),
            params,
            executor: Some("node".into()),
            directory: None,
            tool_timeout_ms: None,
        })
        .await;
    ensure!(response.ok, "{}", response.error.unwrap_or_default());
    response.result.ok_or_else(|| anyhow!("no result"))
}
async fn pty(client: &Client, bin: &str, pid: &str, flat: bool) -> Result<Value> {
    let run=rpc(client,"exbash",json!({"mode":"run","command":format!("{bin} --wait --bytes {PTY_BYTES}{}",if flat{" --flat"}else{""}),"read_timeout":0,"timeout":30000})).await?;
    let id = run["metadata"]["asyncID"].as_str().unwrap();
    let mut terminal = client
        .open_terminal(Some("node"), id, "encryption-bench", 80, 24)
        .await?;
    loop {
        if let Some(Frame::Text(text)) = terminal.link().recv().await? {
            match serde_json::from_str::<ServerText>(&text)? {
                ServerText::Meta { role, .. } if role == "Controller" => break,
                ServerText::Error { message } => return Err(anyhow!(message)),
                _ => {}
            }
        }
    }
    let sample = Sample::new(pid);
    terminal
        .link()
        .send(Frame::Binary(b"go\n".to_vec()))
        .await?;
    let mut received = 0;
    let mut xs = 0;
    let mut tail = Vec::new();
    loop {
        match terminal.link().recv().await? {
            Some(Frame::Binary(chunk)) => {
                received += chunk.len();
                xs += chunk.iter().filter(|&&b| b == b'x').count();
                tail.extend_from_slice(&chunk);
                if tail.len() > 1024 {
                    tail.drain(..tail.len() - 1024);
                }
                if tail.windows(11).any(|b| b == b"FLOOD_DONE ") {
                    break;
                }
            }
            Some(Frame::Text(text)) => {
                if let ServerText::Error { message } = serde_json::from_str(&text)? {
                    return Err(anyhow!(message));
                }
            }
            _ => return Err(anyhow!("PTY incomplete")),
        }
    }
    let expected = if flat { PTY_BYTES } else { PTY_BYTES / 80 * 78 };
    ensure!(xs == expected, "PTY corrupt {xs}/{expected}");
    let mut value = sample.result(if flat { "pty-flat" } else { "pty-short-lines" }, PTY_BYTES);
    value["receivedBytes"] = json!(received);
    value["validatedX"] = json!(xs);
    let _ = rpc(client, "exbash", json!({"mode":"remove","asyncID":id})).await;
    Ok(value)
}
async fn pty_latency(client: &Client) -> Result<Value> {
    let run = rpc(
        client,
        "exbash",
        json!({"mode":"run","command":"sh -c 'stty -echo; cat'","read_timeout":0}),
    )
    .await?;
    let id = run["metadata"]["asyncID"].as_str().unwrap();
    let mut terminal = client
        .open_terminal(Some("node"), id, "latency", 80, 24)
        .await?;
    loop {
        if let Some(Frame::Text(text)) = terminal.link().recv().await? {
            if matches!(
                serde_json::from_str::<ServerText>(&text)?,
                ServerText::Meta { .. }
            ) {
                break;
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut times = vec![];
    for i in 0..210 {
        let line = format!("sample-{i:08}-{}\n", "a".repeat(112));
        let start = Instant::now();
        terminal
            .link()
            .send(Frame::Binary(line.into_bytes()))
            .await?;
        loop {
            if let Some(Frame::Binary(b)) = terminal.link().recv().await? {
                if b.ends_with(b"\n") {
                    break;
                }
            }
        }
        if i >= 10 {
            times.push(start.elapsed().as_secs_f64() * 1e6);
        }
    }
    times.sort_by(f64::total_cmp);
    let _ = rpc(client, "exbash", json!({"mode":"remove","asyncID":id})).await;
    Ok(
        json!({"kind":"pty-latency","samples":times.len(),"p50Us":times[times.len()/2],"p95Us":times[times.len()*95/100],"p99Us":times[times.len()*99/100],"complete":true}),
    )
}
async fn files(client: &Client, root: &PathBuf, pid: &str, upload: bool) -> Result<Value> {
    let mode = if upload { "upload" } else { "download" };
    let options:FileTransferOptions=serde_json::from_value(json!({"mode":mode,"localPath":root.join(if upload{"source"}else{"copy"}),"targetPath":root.join("remote"),"overwrite":true})).unwrap();
    let sample = Sample::new(pid);
    let result = client
        .transfer_file(Some("node"), options, None, None)
        .await?;
    ensure!(result.bytes == BULK_BYTES as u64, "file incomplete");
    let mut value = sample.result(
        if upload {
            "file-upload"
        } else {
            "file-download"
        },
        BULK_BYTES,
    );
    value["sha256"] = json!(result.sha256);
    Ok(value)
}
async fn forward(client: &Client, pid: &str) -> Result<Value> {
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let target = sink.local_addr()?;
    let receiver = tokio::spawn(async move {
        let (mut socket, _) = sink.accept().await.unwrap();
        let mut buf = vec![0u8; 65536];
        let mut count = 0;
        let mut valid = true;
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            valid &= buf[..n].iter().all(|b| *b == 0x5a);
            count += n;
        }
        socket
            .write_all(&(count as u64).to_be_bytes())
            .await
            .unwrap();
        (count, valid)
    });
    let f=rpc(client,"forward",json!({"mode":"start","listen":"127.0.0.1:0","target":{"kind":"tcp","address":target.to_string()}})).await?;
    let id = f["metadata"]["forwardID"].as_str().unwrap();
    let listen = f["metadata"]["listen"].as_str().unwrap();
    let sample = Sample::new(pid);
    let mut socket =
        tokio::net::TcpStream::connect(listen.strip_prefix("tcp://").unwrap_or(listen)).await?;
    socket.set_nodelay(true)?;
    let block = vec![0x5a; 65536];
    for _ in 0..BULK_BYTES / block.len() {
        socket.write_all(&block).await?;
    }
    socket.shutdown().await?;
    let mut ack = [0; 8];
    socket.read_exact(&mut ack).await?;
    ensure!(
        u64::from_be_bytes(ack) == BULK_BYTES as u64,
        "forward ACK incomplete"
    );
    let mut result = sample.result("forward-tcp", BULK_BYTES);
    let (count, valid) = receiver.await?;
    ensure!(valid && count == BULK_BYTES, "forward corruption");
    result["validatedBytes"] = json!(count);
    rpc(client, "forward", json!({"mode":"stop","forwardID":id})).await?;
    Ok(result)
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let url = &args[1];
    let root = PathBuf::from(&args[2]);
    let pid = &args[3];
    let config = &args[4];
    let secure = url.starts_with("tls:") || url.starts_with("wss:");
    let mut options = json!({"id":"node","url":url,"username":"bench"});
    if secure {
        options["authentication"] = json!({"method":"password","password":"benchmark-password"});
        options["tls"] = json!({"caFile":format!("{config}/server-cert.pem")});
    }
    let client = Client::new().await?;
    let start = Instant::now();
    client
        .connect_to_executor(serde_json::from_value(options)?)
        .await?;
    println!(
        "{}",
        json!({"kind":"cold-connect","milliseconds":start.elapsed().as_secs_f64()*1000.0})
    );
    let mut timings = Vec::new();
    for i in 0..110 {
        let start = Instant::now();
        rpc(&client, "client_resources", json!({})).await?;
        if i >= 10 {
            timings.push(start.elapsed().as_secs_f64() * 1e6);
        }
    }
    timings.sort_by(f64::total_cmp);
    println!(
        "{}",
        json!({"kind":"query-latency","p50Us":timings[50],"p95Us":timings[95],"p99Us":timings[99]})
    );
    let mut source = tokio::fs::File::create(root.join("source")).await?;
    let block = vec![0x5a; 1024 * 1024];
    for _ in 0..BULK_BYTES / block.len() {
        source.write_all(&block).await?;
    }
    source.flush().await?;
    drop(source);
    for round in 0..4 {
        for flat in [false, true] {
            let mut r = pty(
                &client,
                &format!("{}/stdout-flood", root.display()),
                pid,
                flat,
            )
            .await?;
            r["round"] = json!(round);
            println!("{r}");
        }
        for upload in [true, false] {
            let mut r = files(&client, &root, pid, upload).await?;
            r["round"] = json!(round);
            println!("{r}");
        }
        let mut r = forward(&client, pid).await?;
        r["round"] = json!(round);
        println!("{r}");
    }
    println!("{}", pty_latency(&client).await?);
    client.shutdown().await;
    Ok(())
}

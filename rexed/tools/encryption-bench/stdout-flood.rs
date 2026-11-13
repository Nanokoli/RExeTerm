//! Native stdout stress generator. Build: rustc -O stdout-flood.rs -o stdout-flood
use std::io::{self, BufRead, Write};
use std::time::Instant;

fn main() {
    if let Err(error) = run() {
        if error.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("stdout-flood: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> io::Result<()> {
    let mut bytes: Option<u64> = None;
    let mut block_size = 65_536usize;
    let mut flat = false;
    let mut wait = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bytes" => bytes = Some(number(args.next(), "--bytes")?),
            "--block" => {
                block_size = usize::try_from(number(args.next(), "--block")?)
                    .map_err(|_| invalid("block size is too large"))?;
                if !(1..=16 * 1024 * 1024).contains(&block_size) {
                    return Err(invalid("--block must be between 1 and 16777216"));
                }
            }
            "--flat" => flat = true,
            "--wait" => wait = true,
            "-h" | "--help" => {
                eprintln!("Usage: stdout-flood [--bytes N] [--block N] [--flat] [--wait]\n\
                    Default: continuous output, blocks of at most 64 KiB, 78 x + CRLF per line.\n\
                    --bytes N  Stop after N payload bytes; default runs until killed.\n\
                    --block N  Maximum write size, 1 byte to 16 MiB.\n\
                    --flat     Only x bytes, without line breaks.\n\
                    --wait     Wait for one input line before emitting payload.\n\
                    Rate and completion are reported on stderr; stdout is payload only.");
                return Ok(());
            }
            _ => return Err(invalid(&format!("unknown argument: {arg}"))),
        }
    }
    // Whole lines keep the pattern continuous across writes.
    if !flat && block_size >= 80 {
        block_size -= block_size % 80;
    }
    let line = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n";
    let block: Vec<u8> = (0..block_size)
        .map(|n| if flat { b'x' } else { line[n % 80] })
        .collect();
    if wait {
        eprintln!("FLOOD_WAITING: send a line to start");
        let mut trigger = String::new();
        if io::stdin().lock().read_line(&mut trigger)? == 0 {
            return Err(invalid("stdin closed before start trigger"));
        }
    }
    let mut output = io::stdout().lock();
    let start = Instant::now();
    let mut written = 0u64;
    while bytes.is_none_or(|limit| written < limit) {
        let count = bytes
            .map(|limit| (limit - written).min(block.len() as u64) as usize)
            .unwrap_or(block.len());
        output.write_all(&block[..count])?;
        written += count as u64;
    }
    output.flush()?;
    let seconds = start.elapsed().as_secs_f64();
    eprintln!(
        "\r\nFLOOD_DONE bytes={written} seconds={seconds:.6} MB/s={:.2}",
        written as f64 / seconds / 1_000_000.0
    );
    Ok(())
}

fn number(value: Option<String>, flag: &str) -> io::Result<u64> {
    value
        .ok_or_else(|| invalid(&format!("missing value for {flag}")))?
        .parse()
        .map_err(|_| invalid(&format!("{flag} requires an unsigned byte count")))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

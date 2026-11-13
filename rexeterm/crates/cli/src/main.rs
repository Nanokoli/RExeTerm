use clap::Parser;
use rexeterm_cli::{
    commands::{self, Args},
    markdown,
};
mod host;
use std::{
    io::{self, Write},
    process::ExitCode,
    time::Duration,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match Args::try_parse_from(rexeterm_cli::commands::normalize_argv(
        std::env::args_os(),
    )) {
        Ok(args) => args,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                print!("{error}");
                return ExitCode::SUCCESS;
            }
            eprint!("{}", markdown::error(&error.to_string()));
            return ExitCode::from(2);
        }
    };
    // Where .rexeterm/ lives is never a CLI argument: use cd, or set REXETERM_CWD
    // for callers that cannot change the process directory themselves.
    if let Some(directory) = std::env::var_os("REXETERM_CWD") {
        if !directory.is_empty() {
            if let Err(error) = std::env::set_current_dir(&directory) {
                eprint!(
                    "{}",
                    markdown::error(&format!(
                        "cannot enter REXETERM_CWD {}: {error}",
                        std::path::Path::new(&directory).display()
                    ))
                );
                return ExitCode::from(2);
            }
        }
    }
    // `-raw FILE` / `-png FILE` imply their sink; the path is used verbatim.
    let mut sinks = args.sink.clone();
    for (path, sink) in [
        (args.raw.as_ref(), markdown::Sink::Raw),
        (args.png.as_ref(), markdown::Sink::Png),
    ] {
        if path.is_some() && !sinks.contains(&sink) {
            sinks.push(sink);
        }
    }
    let options = markdown::Options {
        sinks,
        out_dir: args.out_dir.clone(),
        text_limit: args.text_limit,
        raw_path: args.raw.clone(),
        png_path: args.png.clone(),
    };
    let result = if let Some(spec) = args.daemon() {
        // Same executable, host role: stays alive between CLI invocations.
        host::run(spec).await
    } else {
        let timeout = Duration::from_millis(args.timeout_ms);
        let work = tokio::time::timeout(timeout, commands::execute(args));
        tokio::select! {
            result = work => result.unwrap_or_else(|_| Err(anyhow::anyhow!("CLI request timed out; the host may still be processing it. Inspect tasks/transfers/forwards before retrying a write."))),
            signal = tokio::signal::ctrl_c() => signal.and_then(|_| Err(io::Error::new(io::ErrorKind::Interrupted, "CLI request cancelled; the host operation may still be active"))).map_err(Into::into),
        }
    };
    match result {
        Ok(value) => {
            let output = markdown::render_with(&value, &options);
            match io::stdout().lock().write_all(output.as_bytes()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            }
        }
        Err(error) => {
            if error
                .downcast_ref::<rexeterm_cli::directory::Disabled>()
                .is_some()
            {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
            let _ = io::stderr()
                .lock()
                .write_all(markdown::error(&format!("{error:#}")).as_bytes());
            ExitCode::FAILURE
        }
    }
}

use clap::Parser;
use rexeterm_cli::{
    commands::{self, Args},
    host, markdown,
};
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
                let _ = io::stdout().lock().write_all(error.to_string().as_bytes());
                return ExitCode::SUCCESS;
            }
            let note = match error.kind() {
                    clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument =>
                        "\nCurrent forms: --cmd COMMAND, -A ASYNCID, -f PORT:TARGET_HOST@ONLINE_EXECUTOR:TARGET_PORT, executor connect, service bind ID, and file upload/download REMOTE LOCAL.",
                    _ => "",
                };
            eprint!("{}", markdown::error(&format!("{error}{note}")));
            return ExitCode::from(2);
        }
    };
    // Where the project binding and client preferences live: use cd or REXETERM_CWD
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
    let options = markdown::Options {
        sinks: args.selected_sinks(),
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
            result = work => result.unwrap_or_else(|_| Err(anyhow::anyhow!("CLI request timed out; the host may still be processing it. Inspect connections, tasks, transfers or forwards before retrying a write."))),
            signal = tokio::signal::ctrl_c() => signal.and_then(|_| Err(io::Error::new(io::ErrorKind::Interrupted, "CLI request cancelled; the host operation may still be active"))).map_err(Into::into),
        }
    };
    match result.and_then(|value| {
        let card = markdown::render_with(&value, &options)?;
        if options.sinks.contains(&markdown::Sink::Tui) {
            let ansi = value["output"]["ansi"].as_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "rexetermd did not return a PTY screen; verify the bound daemon is up to date"
                )
            })?;
            let status = &value["metadata"];
            let terminal = &status["terminalOutput"];
            if let Some(error) = terminal["error"].as_str() {
                eprint!("{}", markdown::error(error));
            }
            if status["state"] == "stopped" {
                eprintln!("exitCode: {}", status["exitCode"]);
            }
            if options.raw_path.is_none() && options.sinks.contains(&markdown::Sink::Raw)
                || options.png_path.is_none() && options.sinks.contains(&markdown::Sink::Png)
                || options.sinks.contains(&markdown::Sink::File)
            {
                for file in card.lines().filter(|line| line.starts_with(">**file**:")) {
                    eprintln!("{file}");
                }
            }
            Ok(ansi.to_owned())
        } else {
            Ok(card)
        }
    }) {
        Ok(output) => match io::stdout().lock().write_all(output.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
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

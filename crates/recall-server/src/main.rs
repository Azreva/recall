mod settings;

use recall_server::Server;
use std::process::ExitCode;

const USAGE: &str = "Recall — multithreaded in-memory cache server

Usage: recall-server [options]

  --bind ADDRESS:PORT        Loopback listen address (default 127.0.0.1:6379)
  --workers COUNT            Dedicated keyspace owners (1 to 64)
  --io-threads COUNT         Networking runtime threads (default 2)
  --max-memory-mib MIB       Live key/value payload budget, NOT an RSS limit
  --keys-per-worker COUNT    Preallocated key ceiling per owner
  --max-connections COUNT    Maximum simultaneous clients
  --queue-bytes-mib MIB      Pending byte capacity per owner/coordinator
  --env-file PATH           Load a specific environment file (required to exist)
  --no-env-file             Disable automatic .env loading
  --help                    Show this help
  --version                 Show the version

Settings: command line > process environment > .env in working directory > defaults.
Optional authentication: RECALL_PASSWORD in the environment or environment file.
Persistence, TLS, public binds, and automatic eviction are not implemented.
All stored data disappears at process exit.";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.len() == 1 {
        match arguments[0].as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("Recall {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ => {}
        }
    }
    let config = match settings::load(&arguments) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("Recall configuration error: {message}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = config.validate() {
        eprintln!("Recall configuration error: {error}");
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.io_threads)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Recall runtime startup failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async move {
        let workers = config.workers;
        let server = Server::bind(config).await?;
        eprintln!("Recall {} listening on {}; {workers} owners; MEMORY ONLY; experimental, not production-qualified",
            env!("CARGO_PKG_VERSION"), server.local_addr()?);
        server.run(shutdown_signal()).await
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Recall server stopped with an error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => eprintln!("Cannot install shutdown signal handler: {error}; stopping"),
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("Cannot install shutdown signal handler: {error}; stopping");
        }
    }
}

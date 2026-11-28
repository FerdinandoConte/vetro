//! `vetro-engine`: the headless host process (ARCHITECTURE section 5.5). Ctrl-C stops it
//! cleanly: the session is closed, the source thread joined and the telemetry file flushed.

use std::io::Write;
use std::process::ExitCode;
use vetro_engine::Engine;
use vetro_engine::config::{self, USAGE};
use vetro_transport::Identity;

fn main() -> ExitCode {
    let cfg = match config::parse(std::env::args_os().skip(1)) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("vetro-engine: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
        .and_then(|rt| rt.block_on(run(cfg)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vetro-engine: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cfg: config::Config) -> Result<(), String> {
    let path = config::identity_path(|k| std::env::var_os(k))?;
    let dir = path.parent().ok_or("identity path has no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let identity =
        Identity::load_or_generate(&path).map_err(|e| format!("{}: {e}", path.display()))?;

    let engine = Engine::bind(&cfg, &identity).map_err(|e| e.to_string())?;
    let addr = engine.local_addr().map_err(|e| e.to_string())?;
    eprintln!(
        "vetro-engine: listening on {addr}, host fingerprint {}",
        engine.fingerprint()
    );
    if cfg.pair {
        // The QR's content, on stdout only (never in a log): single use, valid 5 minutes.
        let token = engine.start_pairing().map_err(|e| e.to_string())?;
        let uri = engine.pair_uri(&token).map_err(|e| e.to_string())?;
        let mut out = std::io::stdout().lock();
        writeln!(out, "{uri}")
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string())?;
    }
    engine
        .run(async {
            // Without a console there is no Ctrl-C: run until killed (the tray's stop command
            // arrives with the IPC, M1-12).
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await
        .map_err(|e| e.to_string())
}

use std::{env, fs::OpenOptions, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
};
use genetic_cars::{api, model, scheduler, store::Store};
use tower_http::services::{ServeDir, ServeFile};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let mut port = 8501u16;
    let mut data_dir = env::var_os("GENETIC_CARS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default())
                .join("Library/Application Support/Genetic Cars")
        });
    let mut frontend_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend/out");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => port = args.next().context("--port needs a number")?.parse()?,
            "--data-dir" => {
                data_dir = PathBuf::from(args.next().context("--data-dir needs a path")?)
            }
            "--frontend-dir" => {
                frontend_dir = PathBuf::from(args.next().context("--frontend-dir needs a path")?)
            }
            "--export-types" => {
                let path = args.next().context("--export-types needs a path")?;
                std::fs::write(path, model::typescript())?;
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "genetic-cars [--port 8501] [--data-dir PATH] [--frontend-dir PATH]\n               [--export-types PATH]\nRuns on 127.0.0.1. Requires Apple Silicon with Metal."
                );
                return Ok(());
            }
            _ => bail!("unknown argument: {arg}"),
        }
    }
    std::fs::create_dir_all(&data_dir).context("create experiment directory")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join("server.lock"))?;
    lock.try_lock()
        .context("another Genetic Cars server owns this experiment directory")?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
        .await
        .with_context(|| format!("could not listen on 127.0.0.1:{port}"))?;
    let store = Store::open(data_dir.join("race-lab.sqlite3")).await?;
    let jobs = scheduler::start(store).await?;
    let app = Router::new()
        .merge(api::router(jobs.clone())?)
        .fallback_service(
            ServeDir::new(&frontend_dir)
                .not_found_service(ServeFile::new(frontend_dir.join("404.html"))),
        )
        .layer(middleware::from_fn(local_requests_only));
    println!(
        "Genetic Cars is ready at http://127.0.0.1:{port} (GPU {}, data {})",
        jobs.device_name(),
        data_dir.display()
    );
    let shutdown_jobs = jobs.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let ctrl_c = async {
                let _ = tokio::signal::ctrl_c().await;
            };
            let terminate = async {
                if let Ok(mut signal) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    signal.recv().await;
                }
            };
            tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
            shutdown_jobs.shutdown().await;
        })
        .await?;
    drop(lock);
    Ok(())
}

// Only the local same-origin application can issue commands to this single-user service.
async fn local_requests_only(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let host_name = host.split(':').next().unwrap_or("");
    let local_host = matches!(host_name, "127.0.0.1" | "localhost");
    let same_origin = request.headers().get("origin").is_none_or(|v| {
        v.to_str()
            .is_ok_and(|origin| origin == format!("http://{host}"))
    });
    if !local_host || !same_origin {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::from("local same-origin requests only"))
            .unwrap();
    }
    next.run(request).await
}

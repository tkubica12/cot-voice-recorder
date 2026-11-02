use std::error::Error;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use voiceprompt_cloud::bootstrap::Application;

async fn signal() -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result,_=term.recv()=>Ok(())}
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}
async fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!(
            "voice-recorder [api [--host IP] [--port PORT] | worker | cleanup | check-config]\nProduction ports use injected ACA UAMI and existing resources; no resources are created by startup."
        );
        return Ok(());
    }
    let mode = args.first().map_or("api", String::as_str);
    if mode == "check-config" {
        if args.len() != 1 {
            return Err("check-config takes no additional arguments.".into());
        }
        voiceprompt_cloud::config::Config::from_environment()?;
        println!("Configuration is valid; no network, resource or model access performed.");
        return Ok(());
    }
    if !matches!(mode, "api" | "worker" | "cleanup") {
        return Err("Specify api, worker, cleanup or check-config.".into());
    }
    let mut host: IpAddr = "0.0.0.0".parse()?;
    let mut port: u16 = 8000;
    let mut remaining = args.iter().skip(1);
    while let Some(option) = remaining.next() {
        if mode != "api" {
            return Err("Worker and cleanup take no command options.".into());
        }
        let value = remaining.next().ok_or("Missing API option value.")?;
        match option.as_str() {
            "--host" => host = value.parse()?,
            "--port" => port = value.parse()?,
            _ => return Err("Unknown API option.".into()),
        }
    }
    let app = Application::from_environment()?;
    app.storage.check_resources(&app.config).await?;
    match mode {
        "api" => {
            let listener = tokio::net::TcpListener::bind((host, port)).await?;
            app.api.ready.store(true, Ordering::Release);
            let ready = app.api.ready.clone();
            let shutdown = app.api.shutdown.clone();
            axum::serve(listener, voiceprompt_cloud::api::router(app.api))
                .with_graceful_shutdown(async move {
                    if let Err(error) = signal().await {
                        eprintln!("Signal handler failed: {error}");
                    }
                    ready.store(false, Ordering::Release);
                    shutdown.send_replace(true);
                })
                .await?;
        }
        "worker" => {
            let (stop, stopping) = tokio::sync::watch::channel(false);
            let worker = app.worker.run(stopping);
            tokio::pin!(worker);
            tokio::select! {
                result=&mut worker=>result?,
                result=signal()=>{
                    if let Err(error)=result{eprintln!("Signal handler failed: {error}");}
                    stop.send_replace(true);
                    worker.await?;
                }
            }
        }
        "cleanup" => println!(
            "{}",
            serde_json::to_string(&voiceprompt_cloud::cleanup::run(&app.services).await?)?
        ),
        _ => return Err("Unknown runtime mode.".into()),
    }
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("VoicePrompt cloud failed: {error}");
        std::process::exit(1);
    }
}

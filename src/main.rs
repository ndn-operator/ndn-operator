use actix_web::{get, middleware, web::Data, App, HttpRequest, HttpResponse, HttpServer, Responder};
use operator::{self, telemetry, network_controller::{State as NetworkState, run_nw, run_router, run_pod_sync}};
use operator::cert_controller::{State as CertState, run_cert};

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
}

#[get("/")]
async fn index(c: Data<NetworkState>, _req: HttpRequest) -> impl Responder {
    let d = c.diagnostics().await;
    HttpResponse::Ok().json(&d)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;

    // Initiatilize Kubernetes controller state
    let nw_state = NetworkState::default();
    let cert_state = CertState::default();
    let nw_ctrl = run_nw(nw_state.clone());
    let rt_ctrl = run_router(nw_state.clone());
    let pod_sync = run_pod_sync(nw_state.clone());
    let cert_ctrl = run_cert(cert_state.clone());
    let server =  HttpServer::new(move || {
        App::new()
            .app_data(Data::new(nw_state.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
    })
    .bind("0.0.0.0:8080")?
    .shutdown_timeout(5);

    // All runtimes implements graceful shutdown, so poll until all are done
    let (_, _, _, _, server_result) = tokio::join!(nw_ctrl, rt_ctrl, pod_sync, cert_ctrl, server.run());
    server_result?;
    Ok(())
}

use actix_web::{get, middleware, web::Data, App, HttpRequest, HttpResponse, HttpServer, Responder};
use operator::{self, telemetry, network_controller::{State, run_nw, run_router, run_pod_sync}};

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
}

#[get("/")]
async fn index(c: Data<State>, _req: HttpRequest) -> impl Responder {
    let d = c.diagnostics().await;
    HttpResponse::Ok().json(&d)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;

    // Initiatilize Kubernetes controller state
    let state = State::default();
    let nw_ctrl = run_nw(state.clone());
    let rt_ctrl = run_router(state.clone());
    let pod_sync = run_pod_sync(state.clone());
    let server =  HttpServer::new(move || {
        App::new()
            .app_data(Data::new(state.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
    })
    .bind("0.0.0.0:8080")?
    .shutdown_timeout(5);

    // All runtimes implements graceful shutdown, so poll until all are done
    let (_, _, _, server_result) = tokio::join!(nw_ctrl, rt_ctrl, pod_sync, server.run());
    server_result?;
    Ok(())
}

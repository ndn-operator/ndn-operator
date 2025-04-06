use actix_web::{get, middleware, web::Data, App, HttpRequest, HttpResponse, HttpServer, Responder};
pub use controller::{self, telemetry, State};

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
    let nw_ctrl = controller::run_nw(state.clone());
    let rt_ctrl = controller::run_router(state.clone());
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
    let (_, _, server_result) = tokio::join!(nw_ctrl, rt_ctrl, server.run());
    server_result?;
    Ok(())
}
use actix_web::{
    App, HttpRequest, HttpResponse, HttpServer, Responder, get, middleware, web::Data,
};
use clap::{Args, Parser};
use futures::future::join_all;
use operator::{
    self,
    cert_controller::{Diagnostics as CertDiagnostics, State as CertState, run_cert},
    ext_cert_controller::{Diagnostics as ExtCertDiagnostics, State as ExtCertState, run_ext_cert},
    neighbor_controller::{
        Diagnostics as NeighborDiagnostics, State as NeighborState, run_neighbor,
    },
    network_controller::{Diagnostics as NetworkDiagnostics, State as NetworkState, run_nw},
    pod_controller::{Diagnostics as PodDiagnostics, State as PodState, run_pod_sync},
    router_controller::{Diagnostics as RouterDiagnostics, State as RouterState, run_router},
    telemetry,
};
use serde::Serialize;
use serde_with::skip_serializing_none;
use std::{collections::BTreeSet, future::Future, pin::Pin};

#[derive(Parser, Debug)]
#[command(version, about = "Run ndn-operator controllers", long_about = None)]
struct Cli {
    #[command(flatten)]
    controllers: ControllerFlags,
}

#[derive(Args, Debug, Default, Clone, Copy)]
struct ControllerFlags {
    /// Run network controller
    #[arg(long = "nw", action = clap::ArgAction::SetTrue)]
    nw: bool,
    /// Run router controller
    #[arg(long = "rt", action = clap::ArgAction::SetTrue)]
    rt: bool,
    /// Run neighbor controller
    #[arg(long = "neighbor", action = clap::ArgAction::SetTrue, alias = "nl")]
    neighbor: bool,
    /// Run pod-sync controller
    #[arg(long = "pod", action = clap::ArgAction::SetTrue)]
    pod: bool,
    /// Run certificate controller
    #[arg(long = "cert", action = clap::ArgAction::SetTrue)]
    cert: bool,
    /// Run external certificate controller
    #[arg(long = "ext-cert", action = clap::ArgAction::SetTrue)]
    ext_cert: bool,
}

impl ControllerFlags {
    fn selection(self) -> ControllerSet {
        let mut set = ControllerSet::new();
        if self.nw {
            set.insert(Controllers::Network);
        }
        if self.rt {
            set.insert(Controllers::Router);
        }
        if self.neighbor {
            set.insert(Controllers::Neighbor);
        }
        if self.pod {
            set.insert(Controllers::PodSync);
        }
        if self.cert {
            set.insert(Controllers::Certificate);
        }
        if self.ext_cert {
            set.insert(Controllers::ExternalCertificate);
        }
        if set.is_empty() {
            set.extend(Controllers::all());
        }
        set
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Controllers {
    Network,
    Router,
    Neighbor,
    PodSync,
    Certificate,
    ExternalCertificate,
}

impl Controllers {
    fn all() -> [Self; 6] {
        [
            Self::Network,
            Self::Router,
            Self::Neighbor,
            Self::PodSync,
            Self::Certificate,
            Self::ExternalCertificate,
        ]
    }
}

type ControllerSet = BTreeSet<Controllers>;
type ControllerFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone, Default)]
struct ServerState {
    network: Option<NetworkState>,
    router: Option<RouterState>,
    neighbor: Option<NeighborState>,
    pod: Option<PodState>,
    certificate: Option<CertState>,
    external_certificate: Option<ExtCertState>,
}

#[skip_serializing_none]
#[derive(Serialize)]
struct CombinedDiagnostics {
    network: Option<NetworkDiagnostics>,
    router: Option<RouterDiagnostics>,
    neighbor: Option<NeighborDiagnostics>,
    pod: Option<PodDiagnostics>,
    certificate: Option<CertDiagnostics>,
    external_certificate: Option<ExtCertDiagnostics>,
}

impl ServerState {
    async fn diagnostics(&self) -> CombinedDiagnostics {
        let network = match &self.network {
            Some(state) => Some(state.diagnostics().await),
            None => None,
        };
        let router = match &self.router {
            Some(state) => Some(state.diagnostics().await),
            None => None,
        };
        let neighbor = match &self.neighbor {
            Some(state) => Some(state.diagnostics().await),
            None => None,
        };
        let pod = match &self.pod {
            Some(state) => Some(state.diagnostics().await),
            None => None,
        };
        let certificate = match &self.certificate {
            Some(state) => Some(state.diagnostics().await),
            None => None,
        };
        let external_certificate = match &self.external_certificate {
            Some(state) => Some(state.diagnostics().await),
            None => None,
        };

        CombinedDiagnostics {
            network,
            router,
            neighbor,
            pod,
            certificate,
            external_certificate,
        }
    }
}

fn controller_futures(selection: &ControllerSet) -> (Vec<ControllerFuture>, ServerState) {
    let mut futures = Vec::new();
    let mut server_state = ServerState::default();

    if selection.contains(&Controllers::Network) {
        let state = NetworkState::default();
        server_state.network = Some(state.clone());
        futures.push(Box::pin(async move {
            run_nw(state).await;
        }) as ControllerFuture);
    }
    if selection.contains(&Controllers::Router) {
        let state = RouterState::default();
        server_state.router = Some(state.clone());
        futures.push(Box::pin(async move {
            run_router(state).await;
        }) as ControllerFuture);
    }
    if selection.contains(&Controllers::Neighbor) {
        let state = NeighborState::default();
        server_state.neighbor = Some(state.clone());
        futures.push(Box::pin(async move {
            run_neighbor(state).await;
        }) as ControllerFuture);
    }
    if selection.contains(&Controllers::PodSync) {
        let state = PodState::default();
        server_state.pod = Some(state.clone());
        futures.push(Box::pin(async move {
            run_pod_sync(state).await;
        }) as ControllerFuture);
    }
    if selection.contains(&Controllers::Certificate) {
        let state = CertState::default();
        server_state.certificate = Some(state.clone());
        futures.push(Box::pin(async move {
            run_cert(state).await;
        }) as ControllerFuture);
    }
    if selection.contains(&Controllers::ExternalCertificate) {
        let state = ExtCertState::default();
        server_state.external_certificate = Some(state.clone());
        futures.push(Box::pin(async move {
            run_ext_cert(state).await;
        }) as ControllerFuture);
    }

    (futures, server_state)
}

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
}

#[get("/")]
async fn index(state: Data<ServerState>, _req: HttpRequest) -> impl Responder {
    let diagnostics = state.diagnostics().await;
    HttpResponse::Ok().json(diagnostics)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    telemetry::init().await;

    let controllers = cli.controllers.selection();

    let (controller_futures, server_state) = controller_futures(&controllers);
    let controllers_task = async {
        join_all(controller_futures).await;
    };
    let server_state = Data::new(server_state);
    let server = HttpServer::new(move || {
        App::new()
            .app_data(server_state.clone())
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
    })
    .bind("0.0.0.0:8080")?
    .shutdown_timeout(5);

    // All runtimes implements graceful shutdown, so poll until all are done
    let (_, server_result) = tokio::join!(controllers_task, server.run());
    server_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ControllerFlags, ControllerSet, Controllers, controller_futures};
    use std::collections::BTreeSet;

    #[test]
    fn selection_defaults_to_all_enabled() {
        let selection = ControllerFlags::default().selection();
        assert_eq!(selection, all_controllers());
    }

    #[test]
    fn selection_respects_subset() {
        let flags = ControllerFlags {
            rt: true,
            cert: true,
            ..Default::default()
        };
        let selection = flags.selection();
        let expected = BTreeSet::from([Controllers::Router, Controllers::Certificate]);
        assert_eq!(selection, expected);
    }

    #[tokio::test]
    async fn diagnostics_reflect_selected_controllers() {
        let selection = BTreeSet::from([Controllers::Router, Controllers::ExternalCertificate]);
        let (_, server_state) = controller_futures(&selection);

        let diagnostics = server_state.diagnostics().await;

        assert!(diagnostics.router.is_some());
        assert!(diagnostics.external_certificate.is_some());
        assert!(diagnostics.network.is_none());
        assert!(diagnostics.pod.is_none());
    }

    fn all_controllers() -> ControllerSet {
        Controllers::all().into_iter().collect()
    }
}

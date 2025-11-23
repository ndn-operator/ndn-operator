use actix_web::{
    App, HttpRequest, HttpResponse, HttpServer, Responder, get, middleware, web::Data,
};
use clap::{Args, Parser};
use futures::future::join_all;
use operator::{
    self,
    cert_controller::{State as CertState, run_cert},
    ext_cert_controller::{State as ExtCertState, run_ext_cert},
    neighbor_controller::{State as NeighborState, run_neighbor},
    network_controller::{State as NetworkState, run_nw},
    pod_controller::{State as PodState, run_pod_sync},
    router_controller::{State as RouterState, run_router},
    telemetry,
};
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

impl Controllers {
    fn spawn(self) -> ControllerFuture {
        match self {
            Controllers::Network => Box::pin(async move {
                run_nw(NetworkState::default()).await;
            }),
            Controllers::Router => Box::pin(async move {
                run_router(RouterState::default()).await;
            }),
            Controllers::Neighbor => Box::pin(async move {
                run_neighbor(NeighborState::default()).await;
            }),
            Controllers::PodSync => Box::pin(async move {
                run_pod_sync(PodState::default()).await;
            }),
            Controllers::Certificate => Box::pin(async move {
                run_cert(CertState::default()).await;
            }),
            Controllers::ExternalCertificate => Box::pin(async move {
                run_ext_cert(ExtCertState::default()).await;
            }),
        }
    }
}

fn controller_futures(selection: &ControllerSet) -> Vec<ControllerFuture> {
    selection
        .iter()
        .map(|controller| controller.spawn())
        .collect()
}

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
    let cli = Cli::parse();
    telemetry::init().await;

    let controllers = cli.controllers.selection();

    let controller_futures = controller_futures(&controllers);
    let controllers_task = async {
        join_all(controller_futures).await;
    };
    let server_state = NetworkState::default();
    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(server_state.clone()))
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
    use super::{ControllerFlags, ControllerSet, Controllers};
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

    fn all_controllers() -> ControllerSet {
        Controllers::all().into_iter().collect()
    }
}

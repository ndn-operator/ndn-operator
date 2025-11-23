#[macro_export]
macro_rules! controller_scaffold {
    (
        controller_ty: $resource:ty,
        reporter: $reporter:expr,
        run_fn: $run_fn:ident,
        reconcile_fn: $reconcile_fn:path,
        error_policy_fn: $error_policy_fn:ident,
        error_requeue_secs: $requeue_secs:expr,
        api_builder: $api_builder:expr,
        watcher_config: $watcher_config:expr
        $(, preflight: $preflight:expr)?
    ) => {
        #[allow(unused_imports)]
        use futures::StreamExt;

        #[derive(Clone)]
        pub struct Context {
            /// Kubernetes client shared with reconciler helpers
            pub client: kube::Client,
            /// Event recorder for publishing Kubernetes Events
            pub recorder: kube::runtime::events::Recorder,
            /// Diagnostics shared with HTTP server
            pub diagnostics: std::sync::Arc<tokio::sync::RwLock<Diagnostics>>,
        }

        #[derive(Clone, serde::Serialize)]
        pub struct Diagnostics {
            #[serde(deserialize_with = "from_ts")]
            pub last_event: chrono::DateTime<chrono::Utc>,
            #[serde(skip)]
            pub reporter: kube::runtime::events::Reporter,
        }
        impl Default for Diagnostics {
            fn default() -> Self {
                Self {
                    last_event: chrono::Utc::now(),
                    reporter: $reporter.into(),
                }
            }
        }
        impl Diagnostics {
            fn recorder(&self, client: kube::Client) -> kube::runtime::events::Recorder {
                kube::runtime::events::Recorder::new(client, self.reporter.clone())
            }
        }

        #[derive(Clone, Default)]
        pub struct State {
            diagnostics: std::sync::Arc<tokio::sync::RwLock<Diagnostics>>,
        }

        impl State {
            pub async fn diagnostics(&self) -> Diagnostics {
                self.diagnostics.read().await.clone()
            }

            pub async fn to_context(&self, client: kube::Client) -> std::sync::Arc<Context> {
                std::sync::Arc::new(Context {
                    client: client.clone(),
                    recorder: self.diagnostics.read().await.recorder(client),
                    diagnostics: self.diagnostics.clone(),
                })
            }
        }

        fn $error_policy_fn(
            _: std::sync::Arc<$resource>,
            error: &$crate::Error,
            _: std::sync::Arc<Context>,
        ) -> kube::runtime::controller::Action {
            tracing::warn!("reconcile failed: {:?}", error);
            kube::runtime::controller::Action::requeue(
                std::time::Duration::from_secs(($requeue_secs) as u64),
            )
        }

        pub async fn $run_fn(state: State) {
            let client = kube::Client::try_default()
                .await
                .expect("Expected a valid KUBECONFIG environment variable");
            let api: kube::Api<$resource> = ($api_builder)(client.clone());
                $( ($preflight)(api.clone()).await; )?
            kube::runtime::controller::Controller::new(api, $watcher_config)
                .shutdown_on_signal()
                .run(
                    $reconcile_fn,
                    $error_policy_fn,
                    state.to_context(client.clone()).await,
                )
                .filter_map(async |x| std::result::Result::ok(x))
                .for_each(async |_| ())
                .await;
        }
    };
}

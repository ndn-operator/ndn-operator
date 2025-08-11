use k8s_openapi::api::apps::v1::DaemonSet;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use operator::network_controller::daemonset::create_owned_daemonset;
use operator::network_controller::{NdndSpec, Network, NetworkSpec, OperatorSpec};

fn test_network_spec() -> NetworkSpec {
    NetworkSpec {
        prefix: "/test".into(),
        udp_unicast_port: 6363,
        node_selector: None,
        ndnd: NdndSpec::default(),
        operator: Some(OperatorSpec {
            image: "test/operator:latest".into(),
        }),
        router_cert_issuer: None,
        trust_anchors: None,
    }
}

fn test_network() -> Network {
    let mut nw = Network::new("test-net", test_network_spec());
    nw.metadata = ObjectMeta {
        namespace: Some("test-ns".into()),
        uid: Some("dummy-uid".into()),
        ..ObjectMeta::default()
    };
    nw
}

#[test]
fn daemonset_builder_basic() {
    let nw = test_network();
    let ds: DaemonSet =
        create_owned_daemonset(&nw, Some("opimg".into()), Some("sa".into())).expect("build ds");
    let tmpl = ds.spec.unwrap().template;
    let spec = tmpl.spec.unwrap();
    // Assert we have 2 containers + 1 init
    assert_eq!(spec.containers.len(), 2);
    let init_vec = spec.init_containers.as_ref().expect("init containers");
    assert!(!init_vec.is_empty());
    // Ensure env var presence
    let init = init_vec.first().cloned();
    if let Some(init_ct) = init {
        let env_names: Vec<String> = init_ct
            .env
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(env_names.contains(&"NDN_NETWORK_NAME".to_string()));
        assert!(env_names.contains(&"NDN_SOCKET_PATH".to_string()));
    }
}

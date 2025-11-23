use std::collections::BTreeMap;

use k8s_openapi::api::{
    apps::v1::{DaemonSet, DaemonSetSpec},
    core::v1::{
        Container, ContainerPort, EnvVar, EnvVarSource, HostPathVolumeSource, ObjectFieldSelector,
        PodSpec, PodTemplateSpec, SecurityContext, Volume, VolumeMount,
    },
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::{Resource, ResourceExt};

use crate::{
    Result,
    network_controller::{
        CONTAINER_CONFIG_DIR, CONTAINER_KEYS_DIR, CONTAINER_SOCKET_DIR, DS_LABEL_KEY, Network,
    },
};

/// Build the DaemonSet managed by a Network CR.
pub fn create_owned_daemonset(
    nw: &Network,
    image: Option<String>,
    service_account: Option<String>,
) -> Result<DaemonSet> {
    let oref_opt = nw.controller_owner_ref(&());
    let mut labels = BTreeMap::new();
    labels.insert(DS_LABEL_KEY.to_string(), nw.name_any());
    let container_config_path = nw.container_config_path();
    let container_socket_path = nw.container_socket_path();
    Ok(DaemonSet {
        metadata: ObjectMeta {
            name: Some(nw.name_any()),
            owner_references: oref_opt.map(|o| vec![o]),
            labels: Some(labels.clone()),
            ..ObjectMeta::default()
        },
        spec: Some(DaemonSetSpec {
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..LabelSelector::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    ..ObjectMeta::default()
                }),
                spec: Some(PodSpec {
                    service_account_name: service_account,
                    host_network: Some(true),
                    dns_policy: Some("ClusterFirstWithHostNet".to_string()),
                    node_selector: nw.spec.node_selector.clone(),
                    init_containers: Some(vec![Container {
                        name: "init".to_string(),
                        image: image.clone(),
                        command: vec![
                            "/init".to_string(),
                            "--output".to_string(),
                            container_config_path.clone(),
                        ]
                        .into(),
                        env: Some(network_init_env(nw, &container_socket_path)),
                        security_context: Some(SecurityContext {
                            privileged: Some(true),
                            ..SecurityContext::default()
                        }),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: "config".to_string(),
                                mount_path: CONTAINER_CONFIG_DIR.to_string(),
                                read_only: Some(false),
                                ..VolumeMount::default()
                            },
                            VolumeMount {
                                name: "keys".to_string(),
                                mount_path: CONTAINER_KEYS_DIR.to_string(),
                                read_only: Some(false),
                                ..VolumeMount::default()
                            },
                        ]),
                        ..Container::default()
                    }]),
                    containers: vec![
                        Container {
                            name: "network".to_string(),
                            image: Some(nw.spec.ndnd.clone().image),
                            command: vec!["/ndnd".to_string()].into(),
                            args: Some(vec![
                                "daemon".to_string(),
                                container_config_path.to_string(),
                            ]),
                            security_context: Some(SecurityContext {
                                privileged: Some(true),
                                ..SecurityContext::default()
                            }),
                            ports: {
                                let mut ports = vec![ContainerPort {
                                    container_port: nw.spec.udp_unicast_port as i32,
                                    host_port: Some(nw.spec.udp_unicast_port as i32),
                                    protocol: Some("UDP".to_string()),
                                    name: Some("udp".to_string()),
                                    ..ContainerPort::default()
                                }];
                                if let Some(faces) = nw.spec.faces.as_ref() {
                                    if let Some(tcp) = faces.tcp.as_ref() {
                                        ports.push(ContainerPort {
                                            container_port: tcp.port as i32,
                                            host_port: Some(tcp.port as i32),
                                            protocol: Some("TCP".to_string()),
                                            name: Some("tcp".to_string()),
                                            ..ContainerPort::default()
                                        });
                                    }
                                    if let Some(ws) = faces.websocket.as_ref() {
                                        ports.push(ContainerPort {
                                            container_port: ws.port as i32,
                                            host_port: Some(ws.port as i32),
                                            protocol: Some("TCP".to_string()),
                                            name: Some("websocket".to_string()),
                                            ..ContainerPort::default()
                                        });
                                    }
                                }
                                Some(ports)
                            },
                            env: Some(vec![EnvVar {
                                name: "NDN_CLIENT_TRANSPORT".to_string(),
                                value: Some(format!("unix://{container_socket_path}")),
                                ..EnvVar::default()
                            }]),
                            volume_mounts: Some(vec![
                                VolumeMount {
                                    name: "config".to_string(),
                                    mount_path: CONTAINER_CONFIG_DIR.to_string(),
                                    read_only: Some(true),
                                    ..VolumeMount::default()
                                },
                                VolumeMount {
                                    name: "run-ndnd".to_string(),
                                    mount_path: CONTAINER_SOCKET_DIR.to_string(),
                                    ..VolumeMount::default()
                                },
                                VolumeMount {
                                    name: "keys".to_string(),
                                    mount_path: CONTAINER_KEYS_DIR.to_string(),
                                    read_only: Some(false),
                                    ..VolumeMount::default()
                                },
                            ]),
                            ..Container::default()
                        },
                        Container {
                            name: "watch".to_string(),
                            image,
                            command: vec!["/sidecar".to_string()].into(),
                            env: Some(vec![
                                EnvVar {
                                    name: "NDN_NETWORK_NAME".to_string(),
                                    value: Some(nw.name_any()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "LOG".to_string(),
                                    value: Some("debug".to_string()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_NETWORK_NAMESPACE".to_string(),
                                    value_from: Some(EnvVarSource {
                                        field_ref: Some(ObjectFieldSelector {
                                            field_path: "metadata.namespace".to_string(),
                                            ..ObjectFieldSelector::default()
                                        }),
                                        ..EnvVarSource::default()
                                    }),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_ROUTER_NAME".to_string(),
                                    value_from: Some(EnvVarSource {
                                        field_ref: Some(ObjectFieldSelector {
                                            field_path: "metadata.name".to_string(),
                                            ..ObjectFieldSelector::default()
                                        }),
                                        ..EnvVarSource::default()
                                    }),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_CLIENT_TRANSPORT".to_string(),
                                    value: Some(format!("unix://{container_socket_path}")),
                                    ..EnvVar::default()
                                },
                            ]),
                            volume_mounts: Some(vec![VolumeMount {
                                name: "run-ndnd".to_string(),
                                mount_path: CONTAINER_SOCKET_DIR.to_string(),
                                ..VolumeMount::default()
                            }]),
                            ..Container::default()
                        },
                    ],
                    volumes: Some(vec![
                        Volume {
                            name: "config".to_string(),
                            host_path: Some(HostPathVolumeSource {
                                path: nw.host_config_dir(),
                                type_: Some("DirectoryOrCreate".to_string()),
                            }),
                            ..Volume::default()
                        },
                        Volume {
                            name: "run-ndnd".to_string(),
                            host_path: Some(HostPathVolumeSource {
                                path: nw.host_socket_dir(),
                                type_: Some("DirectoryOrCreate".to_string()),
                            }),
                            ..Volume::default()
                        },
                        Volume {
                            name: "keys".to_string(),
                            host_path: Some(HostPathVolumeSource {
                                path: nw.host_keys_dir(),
                                type_: Some("DirectoryOrCreate".to_string()),
                            }),
                            ..Volume::default()
                        },
                    ]),
                    ..PodSpec::default()
                }),
            },
            ..DaemonSetSpec::default()
        }),
        ..Default::default()
    })
}

fn network_init_env(nw: &Network, container_socket_path: &str) -> Vec<EnvVar> {
    let mut envs = vec![
        EnvVar {
            name: "NDN_NETWORK_NAME".into(),
            value: Some(nw.name_any()),
            ..EnvVar::default()
        },
        EnvVar {
            name: "NDN_UDP_UNICAST_PORT".into(),
            value: Some(nw.spec.udp_unicast_port.to_string()),
            ..EnvVar::default()
        },
        EnvVar {
            name: "LOG".into(),
            value: Some("debug".into()),
            ..EnvVar::default()
        },
        EnvVar {
            name: "NDN_NETWORK_NAMESPACE".into(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    field_path: "metadata.namespace".into(),
                    ..ObjectFieldSelector::default()
                }),
                ..EnvVarSource::default()
            }),
            ..EnvVar::default()
        },
        EnvVar {
            name: "NDN_ROUTER_NAME".into(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    field_path: "metadata.name".into(),
                    ..ObjectFieldSelector::default()
                }),
                ..EnvVarSource::default()
            }),
            ..EnvVar::default()
        },
        EnvVar {
            name: "NDN_NODE_NAME".into(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    field_path: "spec.nodeName".into(),
                    ..ObjectFieldSelector::default()
                }),
                ..EnvVarSource::default()
            }),
            ..EnvVar::default()
        },
        EnvVar {
            name: "NDN_SOCKET_PATH".into(),
            value: Some(container_socket_path.to_string()),
            ..EnvVar::default()
        },
        EnvVar {
            name: "NDN_KEYS_DIR".into(),
            value: Some(nw.container_keys_dir()),
            ..EnvVar::default()
        },
        EnvVar {
            name: "NDN_INSECURE".into(),
            value: Some(match nw.spec.router_cert_issuer {
                Some(_) => "false".into(),
                None => "true".into(),
            }),
            ..EnvVar::default()
        },
    ];

    let family = match nw.spec.ip_family {
        crate::network_controller::IpFamily::IPv4 => "IPv4",
        crate::network_controller::IpFamily::IPv6 => "IPv6",
    };
    envs.push(EnvVar {
        name: "NDN_IP_FAMILY".into(),
        value: Some(family.to_string()),
        ..EnvVar::default()
    });

    if let Some(faces) = nw.spec.faces.as_ref() {
        if let Some(tcp) = faces.tcp.as_ref() {
            envs.push(EnvVar {
                name: "NDN_TCP_PORT".into(),
                value: Some(tcp.port.to_string()),
                ..EnvVar::default()
            });
        }
        if let Some(ws) = faces.websocket.as_ref() {
            envs.push(EnvVar {
                name: "NDN_WS_PORT".into(),
                value: Some(ws.port.to_string()),
                ..EnvVar::default()
            });
        }
    }

    if let Some(trust_anchors) = nw.spec.trust_anchors.as_ref()
        && let Ok(serialized) = serde_json::to_string(trust_anchors)
    {
        envs.push(EnvVar {
            name: "NDN_TRUST_ANCHORS".into(),
            value: Some(serialized),
            ..EnvVar::default()
        });
    }

    envs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert_controller::IssuerRef;
    use crate::network_controller::{
        FacesSpec, IpFamily, NdndSpec, NetworkSpec, NetworkTcpFaceSpec, NetworkWebSocketFaceSpec,
        OperatorSpec, TrustAnchorRef,
    };

    fn build_network(with_issuer: bool) -> Network {
        let spec = NetworkSpec {
            prefix: "/example/net".into(),
            udp_unicast_port: 6363,
            ip_family: IpFamily::IPv6,
            node_selector: None,
            ndnd: NdndSpec {
                image: "ndnd:test".into(),
            },
            operator: Some(OperatorSpec {
                image: "op:test".into(),
            }),
            router_cert_issuer: if with_issuer {
                Some(IssuerRef {
                    name: "issuer".into(),
                    kind: "Certificate".into(),
                    namespace: Some("ns".into()),
                })
            } else {
                None
            },
            trust_anchors: Some(vec![TrustAnchorRef {
                name: "root".into(),
                kind: "Certificate".into(),
                namespace: Some("ns".into()),
            }]),
            faces: Some(FacesSpec {
                tcp: Some(NetworkTcpFaceSpec {
                    port: 7000,
                    service_template: Default::default(),
                }),
                websocket: Some(NetworkWebSocketFaceSpec {
                    port: 7001,
                    service_template: Default::default(),
                }),
            }),
        };

        let mut nw = Network::new("demo", spec);
        nw.metadata.namespace = Some("demo-ns".into());
        nw.metadata.uid = Some("uid-123".into());
        nw
    }

    fn env_value(envs: &[EnvVar], key: &str) -> Option<String> {
        envs.iter()
            .find(|e| e.name == key)
            .and_then(|e| e.value.clone())
    }

    #[test]
    fn network_init_env_populates_tcp_ws_and_trust_anchors() {
        let nw = build_network(true);
        let envs = network_init_env(&nw, "/run/demo.sock");
        assert_eq!(
            env_value(&envs, "NDN_NETWORK_NAME").as_deref(),
            Some("demo")
        );
        assert_eq!(
            env_value(&envs, "NDN_SOCKET_PATH").as_deref(),
            Some("/run/demo.sock")
        );
        assert_eq!(env_value(&envs, "NDN_INSECURE").as_deref(), Some("false"));
        assert_eq!(env_value(&envs, "NDN_IP_FAMILY").as_deref(), Some("IPv6"));
        assert_eq!(env_value(&envs, "NDN_TCP_PORT").as_deref(), Some("7000"));
        assert_eq!(env_value(&envs, "NDN_WS_PORT").as_deref(), Some("7001"));
        let trust = env_value(&envs, "NDN_TRUST_ANCHORS").expect("trust anchors env");
        assert!(trust.contains("\"root\""));
    }

    #[test]
    fn network_init_env_marks_insecure_without_issuer() {
        let nw = build_network(false);
        let envs = network_init_env(&nw, "/run/demo.sock");
        assert_eq!(env_value(&envs, "NDN_INSECURE").as_deref(), Some("true"));
        assert!(env_value(&envs, "NDN_TRUST_ANCHORS").is_some());
    }
}

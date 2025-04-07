use crate::crd::Network;
use k8s_openapi::api::apps::v1::{DaemonSet, DaemonSetSpec};
use k8s_openapi::api::core::v1::{Container, ContainerPort, EnvVar, EnvVarSource, HostPathVolumeSource, ObjectFieldSelector, PodSpec, PodTemplateSpec, SecurityContext, Volume, VolumeMount};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::{Resource, ResourceExt};
use std::collections::BTreeMap;

pub fn create_owned_daemonset(source: &Network, image: Option<String>, service_account: Option<String>) -> DaemonSet {
    let oref = source.controller_owner_ref(&()).unwrap();
    let mut labels = BTreeMap::new();
    labels.insert("network".to_string(), source.name_any());
    let config_file_folder = "/etc/ndnd".to_string();
    let config_file_name = format!("{}.yml", source.name_any());
    let config_file_path = format!("{}/{}", config_file_folder, config_file_name);
    let socket_file_folder = "/run/ndnd".to_string();
    let socket_file_name = format!("{}.sock", source.name_any());
    let socket_file_path = format!("{}/{}", socket_file_folder, socket_file_name);
    DaemonSet {
        metadata: ObjectMeta {
            name: Some(source.name_any()),
            owner_references: Some(vec![oref]),
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
                    init_containers: Some(vec![Container {
                        name: "init".to_string(),
                        image: image.clone(),
                        command: vec!["/init".to_string(), "--output".to_string(), config_file_path.clone()].into(),
                        env: Some(vec![
                            EnvVar {
                                name: "NDN_NETWORK_NAME".to_string(),
                                value: Some(source.name_any()),
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
                                        field_path: "spec.nodeName".to_string(),
                                        ..ObjectFieldSelector::default()
                                    }),
                                    ..EnvVarSource::default()
                                }),
                                ..EnvVar::default()
                            },
                            EnvVar {
                                name: "NDN_NODE_NAME".to_string(),
                                value_from: Some(EnvVarSource {
                                    field_ref: Some(ObjectFieldSelector {
                                        field_path: "spec.nodeName".to_string(),
                                        ..ObjectFieldSelector::default()
                                    }),
                                    ..EnvVarSource::default()
                                }),
                                ..EnvVar::default()
                            },
                            EnvVar {
                                name: "NDN_SOCKET_PATH".to_string(),
                                value: Some(socket_file_path.clone()),
                                ..EnvVar::default()
                            },
                        ]),
                        security_context: Some(SecurityContext {
                            privileged: Some(true),
                            ..SecurityContext::default()
                        }),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: "config".to_string(),
                                mount_path: config_file_folder.clone(),
                                read_only: Some(false),
                                ..VolumeMount::default()
                            },
                        ]),
                        ..Container::default()
                    }]),
                    containers: vec![Container {
                        name: "network".to_string(),
                        image: Some("ghcr.io/named-data/ndnd:20250405".to_string()),
                        command: vec!["/ndnd".to_string()].into(),
                        args: Some(vec!["daemon".to_string(), config_file_path]),
                        security_context: Some(SecurityContext {
                            privileged: Some(true),
                            ..SecurityContext::default()
                        }),
                        ports: Some(vec![
                            ContainerPort {
                                container_port: source.spec.udp_unicast_port,
                                host_port: Some(source.spec.udp_unicast_port),
                                protocol: Some("UDP".to_string()),
                                ..ContainerPort::default()
                            },
                        ]),
                        env: Some(vec![
                            EnvVar {
                                name: "NDN_CLIENT_TRANSPORT".to_string(),
                                value: Some(format!("unix://{}", socket_file_path)),
                                ..EnvVar::default()
                            },
                        ]),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: "config".to_string(),
                                mount_path: config_file_folder,
                                read_only: Some(true),
                                ..VolumeMount::default()
                            },
                            VolumeMount {
                                name: "run-ndnd".to_string(),
                                mount_path: socket_file_folder.clone(),
                                ..VolumeMount::default()
                            },
                        ]),
                        ..Container::default()
                    },
                    Container {
                        name: "watch".to_string(),
                        image: image,
                        command: vec!["/sidecar".to_string()].into(),
                        env: Some(vec![
                            EnvVar {
                                name: "NDN_NETWORK_NAME".to_string(),
                                value: Some(source.name_any()),
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
                                        field_path: "spec.nodeName".to_string(),
                                        ..ObjectFieldSelector::default()
                                    }),
                                    ..EnvVarSource::default()
                                }),
                                ..EnvVar::default()
                            },
                            EnvVar {
                                name: "NDN_CLIENT_TRANSPORT".to_string(),
                                value: Some(format!("unix://{}", socket_file_path)),
                                ..EnvVar::default()
                            },
                        ]),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: "run-ndnd".to_string(),
                                mount_path: socket_file_folder,
                                ..VolumeMount::default()
                            },
                        ]),
                        ..Container::default()
                    }],
                    volumes: Some(vec![
                        Volume {
                            name: "config".to_string(),
                            host_path: Some(HostPathVolumeSource {
                                path: "/etc/ndnd".to_string(),
                                type_: Some("DirectoryOrCreate".to_string()),
                                ..HostPathVolumeSource::default()
                            }),
                            ..Volume::default()
                        },
                        Volume {
                            name: "run-ndnd".to_string(),
                            host_path: Some(HostPathVolumeSource {
                                path: "/run/ndnd".to_string(),
                                type_: Some("DirectoryOrCreate".to_string()),
                                ..HostPathVolumeSource::default()
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
    }
}
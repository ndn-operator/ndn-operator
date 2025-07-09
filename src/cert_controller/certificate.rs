use std::{collections::BTreeMap, sync::Arc};
use std::process::Command;
use base64::prelude::*;
use tempfile::NamedTempFile;
use crate::{Error, Result};
use super::Context;
use chrono::{DateTime, Duration, Utc};
use k8s_openapi::api::core::v1::Secret;
use kube::api::PostParams;
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams, ResourceExt}, core::object::HasStatus, runtime::{
        controller::Action,
        events::{Event, EventType},
    }, CustomResource, Resource
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_with::skip_serializing_none;
use tracing::*;

pub static CERTIFICATE_FINALIZER: &str = "certificate.named-data.net/finalizer";
pub static CERTIFICATE_MANAGER_NAME: &str = "cert-controller";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "NDNCertificate", derive="Default", namespaced, shortname = "ndncert")]
#[kube(status = "NDNCertificateStatus")]
pub struct NDNCertificateSpec {
    pub prefix: String,
    pub issuer: IssuerRef,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IssuerRef {
    pub name: String,
    pub kind: String,
    pub namespace: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NDNCertificateStatus {
    pub key: KeyStatus,
    pub cert: CertStatus,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub name: Option<String>,
    pub sig_type: Option<String>,
    pub secret: Option<String>,
    pub created: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertStatus {
    pub name: Option<String>,
    pub sig_type: Option<String>,
    pub signer_key: Option<String>,
    pub validity: Option<(String, String)>, // (start, end)
    pub secret: Option<String>,
    pub valid: bool,
}

impl NDNCertificate {
    const SECRET_KEY: &str = "ndn.key";
    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        debug!("Reconciling Certificate {:?}", self);
        let ns = self.namespace().unwrap();
        let serverside = PatchParams::apply(CERTIFICATE_MANAGER_NAME);
        let api_cert: Api<NDNCertificate> = Api::namespaced(ctx.client.clone(), self.namespace().as_ref().unwrap());
        let api_secret: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let mut status = self.status().cloned().unwrap_or_default();
        
        if !status.key.created {
            debug!("Generating key for Certificate {:?}", self);
            let key_info = generate_key(&self.spec.prefix)?;
            // Create owned secret
            let api_secret: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
            let secret_name = format!("{}-key", self.name_any());
            let secret_data = self.create_owned_secret(secret_name.clone(), &BTreeMap::from([
                (Self::SECRET_KEY.to_string(), key_info.key_text),
            ]));
            let secret = api_secret.create(&PostParams::default(), &secret_data).await
                .map_err(Error::KubeError)?;

            // Publish an event for the created key
            debug!("Created Key Secret: {:?}", secret);
            ctx.recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "KeyCreated".into(),
                        note: Some(format!("Created `{}` Key for `{}` NDNCertificate", secret.name_any(), self.name_any())),
                        action: "Created".into(),
                        secondary: None,
                    },
                    &self.object_ref(&()),
                )
                .await
                .map_err(Error::KubeError)?;

            // Patch the status with the key information
            status.key.name = Some(key_info.name);
            status.key.sig_type = Some(key_info.sig_type);
            status.key.secret = Some(secret.name_any());
            status.key.created = true;

            let patch = Patch::Apply(json!({
                "status": status
            }));

            api_cert.patch_status(self.name_any().as_str(), &serverside, &patch).await
                .map_err(Error::KubeError)?;
        };
        if !status.cert.valid {
            debug!("Signing certificate for Certificate {:?}", self);
            let issuer_ref = &self.spec.issuer;
            // Get the issuer resource
            let key_secret_name = match issuer_ref.kind.as_str() {
                "NDNCertificate" => {
                    let api_issuer: Api<NDNCertificate> = Api::namespaced(ctx.client.clone(), issuer_ref.namespace.as_deref().unwrap_or(&ns));
                    let issuer = api_issuer.get_status(&issuer_ref.name).await.map_err(Error::KubeError)?;
                    let issuer_status = issuer.status.as_ref().ok_or_else(|| Error::OtherError("Issuer status not found".to_string()))?;
                    if !issuer_status.key.created {
                        return Err(Error::OtherError("Issuer key not created".to_string()));
                    }
                    issuer_status.key.secret.as_ref().ok_or_else(|| Error::OtherError("Key secret not found".to_string()))?.clone()
                },
                _ => {
                    return Err(Error::OtherError(format!("Unsupported issuer kind: {}", issuer_ref.kind)));
                }
            };

            let key_secret = api_secret.get(&key_secret_name).await.map_err(Error::KubeError)?;
            let key_data_bytes = key_secret.data.as_ref().and_then(|d| d.get(Self::SECRET_KEY).cloned())
                .ok_or_else(|| Error::OtherError("Key data not found in secret".to_string()))?;

            let key_data = BASE64_STANDARD.decode(key_data_bytes.0)
                .map_err(Error::DecodeError)?;
            let key_text = String::from_utf8(key_data.clone()).map_err(Error::Utf8Error)?;
            // Sign the certificate
            let cert_info = sign_cert(&key_text, &self.spec.prefix, &SignCertParams {
                start: Some(Utc::now()),
                end: Some(Utc::now() + Duration::days(1)), // 24 hours from now
                issuer: Some(self.spec.issuer.name.clone()),
                info: None,
            })?;

            // Create owned secret for the certificate
            let cert_secret_name = format!("{}-cert", self.name_any());
            let cert_data = self.create_owned_secret(cert_secret_name.clone(), &BTreeMap::from([
                ("ndn.cert".to_string(), cert_info.cert_text),
            ]));
            let cert_secret = api_secret.create(&PostParams::default(), &cert_data).await
                .map_err(Error::KubeError)?;

            // Publish an event for the created certificate
            debug!("Created Certificate Secret: {:?}", cert_secret);
            ctx.recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "CertCreated".into(),
                        note: Some(format!("Created `{}` Cert for `{}` NDNCertificate", cert_secret.name_any(), self.name_any())),
                        action: "Created".into(),
                        secondary: None,
                    },
                    &self.object_ref(&()),
                )
                .await
                .map_err(Error::KubeError)?;

            // Patch the status with the certificate information
            status.cert.name = Some(cert_info.name);
            status.cert.sig_type = Some(cert_info.sig_type);
            status.cert.signer_key = Some(cert_info.signer_key);
            status.cert.validity = Some((cert_info.validity.0.to_rfc3339(), cert_info.validity.1.to_rfc3339()));
            status.cert.secret = Some(cert_secret.name_any());
            status.cert.valid = true;

            let patch = Patch::Apply(json!({
                "status": status
            }));

            api_cert.patch_status(self.name_any().as_str(), &serverside, &patch).await
                .map_err(Error::KubeError)?;
        };
        Ok(Action::await_change())
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        debug!("Cleaning up Certificate {:?}", self);
        let oref = self.object_ref(&());
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DeleteRequested".into(),
                    note: Some(format!("Delete `{}`", self.name_any())),
                    action: "Deleting".into(),
                    secondary: None,
                },
                &oref,
            )
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }

    fn create_owned_secret(&self, name: String, data: &BTreeMap<String, String>) -> Secret {
        let oref = self.controller_owner_ref(&()).unwrap();
        Secret {
            metadata: ObjectMeta {
                name: Some(name),
                owner_references: Some(vec![oref]),
                ..ObjectMeta::default()
            },
            string_data: Some(data.clone()),
            ..Secret::default()
        }
    }
}

fn generate_key(prefix: &str) -> Result<KeyInfo, Error> {
    // Generate a new key
    let output = Command::new("/ndnd")
        .arg("sec")
        .arg("keygen")
        .arg(prefix)
        .arg("ecc")
        .arg("secp256r1")
        .output()
        .map_err(Error::IoError)?;

    if !output.status.success() {
        return Err(Error::OtherError(format!(
            "ndnsec keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let key_text = String::from_utf8(output.stdout).map_err(|e| Error::OtherError(e.to_string()))?;
    let key_info = KeyInfo::from_key_text(key_text)?;

    Ok(key_info)
}

struct KeyInfo {
    name: String,
    sig_type: String,
    key_text: String,
}

impl KeyInfo {
    fn from_key_text(key_text: String) -> Result<Self, Error> {
    // Parse the key to get the name and sig_type
    // key format:
    //-----BEGIN NDN KEY-----
    // Name: <prefix>
    // SigType: <sig_type>
    // <key_data>
    // -----END NDN KEY-----
        let mut name = None;
        let mut sig_type = None;
        for line in key_text.lines() {
            if let Some((key, value)) = line.split_once(":") {
                match key {
                    "Name" => name = Some(value.trim().to_string()),
                    "SigType" => sig_type = Some(value.trim().to_string()),
                    _ => {}
                }
            }
        }

        Ok(KeyInfo {
            name: name.ok_or_else(|| Error::OtherError("Could not parse key name".to_string()))?,
            sig_type: sig_type.ok_or_else(|| Error::OtherError("Could not parse sig type".to_string()))?,
            key_text,
        })
    }
}

#[derive(Default)]
struct SignCertParams {
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    issuer: Option<String>,
    info: Option<String>,
}

fn sign_cert(signer_key: &str, cert_key: &str, params: &SignCertParams) -> Result<CertInfo, Error> {
    
    // Create a temporary file with the signer key
    let temp_signer_key_file = NamedTempFile::new().map_err(Error::IoError)?;
    std::fs::write(temp_signer_key_file.path(), signer_key).map_err(Error::IoError)?;
    // Create a temporary file with the cert key
    let temp_cert_key_file = NamedTempFile::new().map_err(Error::IoError)?;
    std::fs::write(temp_cert_key_file.path(), cert_key).map_err(Error::IoError)?;
    
    // Transform the params into command line arguments
    let mut args = vec!["sec", "sign-cert", temp_signer_key_file.path().to_str().unwrap()];
    let start_str;
    if let Some(start) = params.start {
        args.push("--start");
        start_str = start.format("%Y%m%d%H%M%S").to_string();
        args.push(&start_str);
    }
    let end_str;
    if let Some(end) = params.end {
        args.push("--end");
        end_str = end.format("%Y%m%d%H%M%S").to_string();
        args.push(&end_str);
    }
    if let Some(issuer) = &params.issuer {
        args.push("--issuer");
        args.push(issuer);
    }
    if let Some(info) = &params.info {
        args.push("--info");
        args.push(info);
    }
    args.push(temp_cert_key_file.path().to_str().unwrap());
    // cert_key goes to stdin
    let output = Command::new("/ndnd")
        .args(&args)
        .output()
        .map_err(Error::IoError)?;

    if !output.status.success() {
        return Err(Error::OtherError(format!(
            "ndnsec sign failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let cert_text = String::from_utf8(output.stdout).map_err(|e| Error::OtherError(e.to_string()))?;
    Ok(CertInfo::from_cert_text(cert_text)?)
}

struct CertInfo {
    name: String,
    sig_type: String,
    signer_key: String,
    validity: (DateTime<Utc>, DateTime<Utc>),
    cert_text: String,
}

impl CertInfo {
    fn from_cert_text(cert_text: String) -> Result<Self, Error> {
        // Parse the certificate to get the name, sig_type, signer_key, and validity
        // cert format:
        //-----BEGIN NDN CERT-----
        // Name: <prefix>
        // SigType: <sig_type>
        // SignerKey: <signer_key>
        // Validity: 2025-07-08 17:55:07 +0000 UTC - 2026-07-08 17:55:07 +0000 UTC
        // <cert_data>
        // -----END NDN CERT-----
        let mut name = None;
        let mut sig_type = None;
        let mut signer_key = None;
        let mut validity: (DateTime<Utc>, DateTime<Utc>) = (Utc::now(), Utc::now());
        let mut validity_str = None;
        for line in cert_text.lines() {
            if let Some((key, value)) = line.split_once(":") {
                match key {
                    "Name" => name = Some(value.trim().to_string()),
                    "SigType" => sig_type = Some(value.trim().to_string()),
                    "SignerKey" => signer_key = Some(value.trim().to_string()),
                    "Validity" => validity_str = Some(value.trim().to_string()),
                    _ => {}
                }
            }
        }

        if let Some(validity_str) = validity_str {
            let parts: Vec<&str> = validity_str.split(" - ").collect();
            if parts.len() == 2 {
                let start = DateTime::parse_from_str(parts[0], "%Y-%m-%d %H:%M:%S %z %Z")
                    .map_err(|e| Error::OtherError(format!("Failed to parse start date: {}", e)))?
                    .with_timezone(&Utc);
                let end = DateTime::parse_from_str(parts[1], "%Y-%m-%d %H:%M:%S %z %Z")
                    .map_err(|e| Error::OtherError(format!("Failed to parse end date: {}", e)))?
                    .with_timezone(&Utc);
                validity = (start, end);
            }
        }

        Ok(CertInfo {
            name: name.ok_or_else(|| Error::OtherError("Could not parse cert name".to_string()))?,
            sig_type: sig_type.ok_or_else(|| Error::OtherError("Could not parse sig type".to_string()))?,
            signer_key: signer_key.ok_or_else(|| Error::OtherError("Could not parse signer key".to_string()))?,
            validity: validity,
            cert_text,
        })
    }
    
}

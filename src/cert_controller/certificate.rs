use std::{collections::BTreeMap, process::Command, sync::Arc};
use duration_string::DurationString;
use tempfile::NamedTempFile;
use crate::{Error, Result};
use super::Context;
use chrono::{DateTime, Duration, Utc};
use std::time::Duration as StdDuration;
use k8s_openapi::api::core::v1::Secret;
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams, PostParams, ResourceExt}, core::object::HasStatus, runtime::{
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
const NDND_PATH: &str = "/usr/local/bin/ndnd";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Certificate", derive="Default", namespaced, shortname = "ndncert")]
#[kube(status = "CertificateStatus")]
pub struct CertificateSpec {
    pub prefix: String,
    pub issuer: IssuerRef,
    #[schemars(with = "Option<String>")]
    pub renew_before: Option<DurationString>,
    #[schemars(with = "Option<String>")]
    pub renew_interval: Option<DurationString>,
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
pub struct CertificateStatus {
    pub key: KeyStatus,
    pub cert: CertStatus,
    pub key_exists: bool,
    pub cert_exists: bool,
    pub needs_renewal: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub name: Option<String>,
    pub sig_type: Option<String>,
    pub secret: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertStatus {
    pub name: Option<String>,
    pub sig_type: Option<String>,
    pub signer_key: Option<String>,
    pub issued_at: Option<String>,
    pub valid_until: Option<String>,
    pub secret: Option<String>,
    pub valid: bool,
}

impl Certificate {
    const SECRET_KEY: &str = "ndn.key";

    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        debug!("Reconciling Certificate {:?}", self.name_any());
        let ns = self.namespace().unwrap();
        let api_cert: Api<Certificate> = Api::namespaced(ctx.client.clone(), self.namespace().as_ref().unwrap());
        let api_secret: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let status = self.status().cloned().unwrap_or_default();
        
        let new_status = match status.key_exists{
            true => {
                debug!("Key is already created for Certificate {:?}", self.name_any());
                match status.cert_exists {
                    true => {
                        debug!("Cert is already created for Certificate {:?}", self.name_any());
                        match status.needs_renewal {
                            true => {
                                debug!("Cert needs renewal for Certificate {:?}", self.name_any());
                                self.create_cert(ctx, &ns, &status, &api_secret).await?
                            },
                            false => {
                                debug!("Cert does not need renewal for Certificate {:?}", self.name_any());
                                self.validate_cert(&status)?
                            }
                        }
                    },
                    false => {
                        debug!("Cert is not created for Certificate {:?}", self.name_any());
                        self.create_cert(ctx, &ns, &status, &api_secret).await?
                    }
                }
            },
            false => {
                debug!("Key is not created for Certificate {:?}", self.name_any());
                self.create_key(ctx, &status, &api_secret).await?
            }
        };
        
        let serverside = PatchParams::apply(CERTIFICATE_MANAGER_NAME);
        let patch = Patch::Merge(json!({
            "status": new_status
        }));
        api_cert.patch_status(self.name_any().as_str(), &serverside, &patch).await
            .map_err(Error::KubeError)?;
    
        Ok(Action::await_change())
    }

    fn valid_until(&self, status: &CertificateStatus) -> Result<DateTime<Utc>> {
        if let Some(valid_until) = &status.cert.valid_until {
            Ok(DateTime::parse_from_rfc3339(valid_until)
                .map_err(Error::ParseError)?
                .with_timezone(&Utc))
        } else {
            Err(Error::MissingAnnotation("validUntil".into()))
        }
    }

    fn renew_before(&self, status: &CertificateStatus) -> Result<DateTime<Utc>> {
        let renew_before: StdDuration = self.spec.renew_before
            .unwrap_or(DurationString::new(StdDuration::from_secs(60 * 60 * 24 * 7))) // Default to 7 days
            .into();
        Ok(self.valid_until(status)? - Duration::from_std(renew_before).map_err(|e| Error::OtherError(e.to_string()))?)
    }

    fn validate_cert(&self, status: &CertificateStatus) -> Result<CertificateStatus> {
        // Validate the certificate status
        let mut new_status = status.clone();
        let valid_until = self.valid_until(&status)?;
        let now = Utc::now();
        debug!("Cert is valid until: {}", valid_until);
        new_status.cert.valid = valid_until > now;

        let renew_before = self.renew_before(&status)?;
        debug!("Cert should be renewed before: {}", renew_before);
        new_status.needs_renewal = renew_before <= now;
        Ok(new_status)
    }

    async fn create_cert(
        &self,
        ctx: Arc<Context>,
        ns: &str,
        status: &CertificateStatus,
        api_secret: &Api<Secret>,
    ) -> Result<CertificateStatus> {
        debug!("Creating cert for Certificate {:?}", self.name_any());
        let mut new_status = status.clone();
        let issuer_ref = &self.spec.issuer;
        // Get the issuer resource
        let signer_key_secret_name = match issuer_ref.kind.as_str() {
            "Certificate" => {
                let api_issuer: Api<Certificate> = Api::namespaced(ctx.client.clone(), issuer_ref.namespace.as_deref().unwrap_or(&ns));
                let issuer = api_issuer.get_status(&issuer_ref.name).await.map_err(Error::KubeError)?;
                let issuer_status = issuer.status.as_ref().ok_or(Error::OtherError("Issuer status not found".to_string()))?;
                debug!("Issuer Status: {:?}", issuer_status);
                if !issuer_status.key_exists {
                    return Err(Error::OtherError("Issuer key not found".to_string()));
                }
                issuer_status.key.secret.as_ref().ok_or(Error::OtherError("Key secret not found".to_string()))?.clone()
            },
            _ => {
                return Err(Error::OtherError(format!("Unsupported issuer kind: {}", issuer_ref.kind)));
            }
        };

        let signer_key_secret = api_secret.get(&signer_key_secret_name).await.map_err(Error::KubeError)?;
        let signer_key_secret_data = decode(&signer_key_secret);
        let signer_key_data = signer_key_secret_data.get(Self::SECRET_KEY).ok_or( Error::OtherError("Key data not found".to_string()))?;
        let signer_key_text = match signer_key_data {
            Decoded::Utf8(s) => s.clone(),
            Decoded::Bytes(_) => return Err(Error::OtherError("Key data is not UTF-8".to_string())),
        };

        let my_key_secret_name = status.key.secret.clone().ok_or(Error::OtherError("Key secret not found".to_string()))?;
        let my_key_secret = api_secret.get(&my_key_secret_name).await.map_err(Error::KubeError)?;
        let my_key_secret_data = decode(&my_key_secret);
        let my_key_data = my_key_secret_data.get(Self::SECRET_KEY).ok_or(Error::OtherError("Key data not found in my key secret".to_string()))?;
        let my_key_text = match my_key_data {
            Decoded::Utf8(s) => s.clone(),
            Decoded::Bytes(_) => return Err(Error::OtherError("Key data is not UTF-8".to_string())),
        };
        let std_duration: StdDuration = self.spec.renew_interval
            .unwrap_or(DurationString::new(StdDuration::from_secs(60 * 60 * 24 * 30))).into(); // Default to 30 days
        let cert_info = sign_cert(&signer_key_text, &my_key_text, &SignCertParams {
            start: Some(Utc::now()),
            end: Some(Utc::now() + Duration::from_std(std_duration).unwrap_or_default()),
            issuer: Some(self.spec.issuer.name.clone()),
            info: None,
        })?;

        // Create owned secret for the certificate
        let cert_secret_name = format!("{}-cert", self.name_any());
        let cert_data = self.create_owned_secret(cert_secret_name.clone(), &BTreeMap::from([
            ("ndn.cert".to_string(), cert_info.cert_text),
        ]));
        let serverside = PatchParams::apply(CERTIFICATE_MANAGER_NAME);
        let cert_secret = api_secret.patch(&cert_secret_name, &serverside, &Patch::Apply(cert_data)).await
            .map_err(Error::KubeError)?;

        // Publish an event for the created certificate
        debug!("Patched Cert Secret: {:?}", cert_secret.name_any());
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "CertCreated".into(),
                    note: Some(format!("Created `{}` Cert for `{}` Certificate", cert_secret.name_any(), self.name_any())),
                    action: "Created".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;

        // Patch the status with the certificate information
        new_status.cert.name = Some(cert_info.name);
        new_status.cert.sig_type = Some(cert_info.sig_type);
        new_status.cert.signer_key = Some(cert_info.signer_key);
        new_status.cert.issued_at = Some(cert_info.validity.0.to_rfc3339());
        new_status.cert.valid_until = Some(cert_info.validity.1.to_rfc3339());
        new_status.cert.secret = Some(cert_secret.name_any());
        new_status.cert.valid = true;
        new_status.cert_exists = true;
        Ok(new_status)
    }

    async fn create_key(
        &self,
        ctx: Arc<Context>,
        status: &CertificateStatus,
        api_secret: &Api<Secret>,
    ) -> Result<CertificateStatus> {
        debug!("Creating key for Certificate {:?}", self.name_any());
        let key_info = generate_key(&self.spec.prefix)?;
        // Create owned secret for the key
        let secret_name = format!("{}-key", self.name_any());
        let secret_data = self.create_owned_secret(secret_name.clone(), &BTreeMap::from([
            (Self::SECRET_KEY.to_string(), key_info.key_text),
        ]));
        let secret = api_secret.create(&PostParams::default(), &secret_data).await
            .map_err(Error::KubeError)?;

        // Publish an event for the created key
        debug!("Created Key Secret: {:?}", secret.name_any());
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "KeyCreated".into(),
                    note: Some(format!("Created `{}` Key for `{}` Certificate", secret.name_any(), self.name_any())),
                    action: "Created".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;

        // Patch the status with the key information
        let mut new_status = status.clone();
        new_status.key.name = Some(key_info.name);
        new_status.key.sig_type = Some(key_info.sig_type);
        new_status.key.secret = Some(secret.name_any());
        new_status.key_exists = true;
        
        Ok(new_status)
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        debug!("Cleaning up Certificate {:?}", self.name_any());
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
    let output = Command::new(NDND_PATH)
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
    debug!("Running ndnd sec sign-cert with args: {:?}", args);
    // cert_key goes to stdin
    let output = Command::new(NDND_PATH)
        .args(&args)
        .stdin(std::fs::File::open(temp_cert_key_file.path()).map_err(Error::IoError)?)
        .output()
        .map_err(Error::IoError)?;
    debug!("ndnd sec sign-cert output: {:?}", output);
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
        debug!("Parsing certificate text: {}", cert_text);
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

enum Decoded {
    /// Usually secrets are just short utf8 encoded strings
    Utf8(String),
    /// But it's allowed to just base64 encode binary in the values
    #[allow(dead_code)]
    Bytes(Vec<u8>),
}

fn decode(secret: &Secret) -> BTreeMap<String, Decoded> {
    let mut res = BTreeMap::new();
    // Ignoring binary data for now
    if let Some(data) = secret.data.clone() {
        for (k, v) in data {
            if let Ok(b) = std::str::from_utf8(&v.0) {
                res.insert(k, Decoded::Utf8(b.to_string()));
            } else {
                res.insert(k, Decoded::Bytes(v.0));
            }
        }
    }
    res
}

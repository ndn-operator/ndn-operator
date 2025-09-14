use std::{collections::BTreeMap, sync::Arc, time::Duration as StdDuration};

use crate::conditions::Conditions;
use chrono::{DateTime, Duration, Utc};
use duration_string::DurationString;
use k8s_openapi::api::core::v1::Secret;
use kube::core::object::HasStatus;
use kube::runtime::{controller::Action, wait::Condition};
use kube::{
    Resource,
    api::{Api, ObjectMeta, Patch, PatchParams, PostParams, ResourceExt},
};
use serde_json::json;
use tracing::*;

use super::Context;
use super::crypto::{SignCertParams, generate_key, sign_cert};
use super::types::{
    Certificate, CertificateStatus, ExternalCertificate, ExternalCertificateStatus,
};
use crate::{Error, Result, events_helper::emit_info};

pub static CERTIFICATE_MANAGER_NAME: &str = "cert-controller";
pub static EXTERNAL_CERTIFICATE_MANAGER_NAME: &str = "external-cert-controller";

impl Certificate {
    pub const SECRET_KEY: &str = "ndn.key";
    pub const CERT_KEY: &str = "ndn.cert";
    pub const SIGNER_CERT_KEY: &str = "signer.cert";

    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        debug!("Reconciling Certificate {:?}", self.name_any());
        let ns = self.namespace().unwrap();
        let api_cert: Api<Certificate> = Api::namespaced(ctx.client.clone(), &ns);
        let api_secret: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let status = self.status().cloned().unwrap_or_default();
        let mut action = Action::await_change();

        // Start with a working copy and surface RenewalRequired immediately from current status
        let mut _working = status.clone();
        _working.upsert_bool(
            "RenewalRequired",
            _working.needs_renewal,
            if _working.needs_renewal {
                "RenewalWindow"
            } else {
                "NotInWindow"
            },
            None,
            self.metadata.generation.unwrap_or(0),
        );

        let new_status = match (status.key_exists, status.cert_exists, status.needs_renewal) {
            (true, true, true) => {
                // Mark Issuing while we create a new certificate
                _working.upsert_bool(
                    "Issuing",
                    true,
                    "Renewing",
                    Some("Renewal is in progress"),
                    self.metadata.generation.unwrap_or(0),
                );
                self.create_cert(ctx.clone(), &ns, &status, &api_secret)
                    .await?
            }
            (true, true, false) => {
                let valid_until = self.valid_until(&status)?;
                let requeue_duration = (valid_until - Utc::now())
                    .to_std()
                    .map_err(|e| Error::OtherError(e.to_string()))?;
                action = Action::requeue(requeue_duration);
                let mut st = self.validate_cert(&status)?;
                // Update KeyReady and CertReady based on current flags
                set_availability_conditions(self, &mut st);
                st
            }
            (true, false, _) => {
                _working.upsert_bool(
                    "Issuing",
                    true,
                    "Issuing",
                    Some("Issuing certificate"),
                    self.metadata.generation.unwrap_or(0),
                );
                self.create_cert(ctx.clone(), &ns, &status, &api_secret)
                    .await?
            }
            (false, _, _) => self.create_key(ctx.clone(), &status, &api_secret).await?,
        };

        let serverside = PatchParams::apply(CERTIFICATE_MANAGER_NAME);
        let patch = Patch::Merge(json!({ "status": new_status }));
        api_cert
            .patch_status(self.name_any().as_str(), &serverside, &patch)
            .await
            .map_err(Error::KubeError)?;

        Ok(action)
    }

    fn valid_until(&self, status: &CertificateStatus) -> Result<DateTime<Utc>> {
        status
            .cert
            .valid_until
            .as_ref()
            .ok_or_else(|| Error::MissingAnnotation("validUntil".into()))
            .and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map_err(Error::ParseError)
                    .map(|dt| dt.with_timezone(&Utc))
            })
    }

    fn renew_before(&self, status: &CertificateStatus) -> Result<DateTime<Utc>> {
        let renew_before: StdDuration = self
            .spec
            .renew_before
            .unwrap_or(DurationString::new(StdDuration::from_secs(
                60 * 60 * 24 * 7,
            )))
            .into();
        Ok(self.valid_until(status)?
            - Duration::from_std(renew_before).map_err(|e| Error::OtherError(e.to_string()))?)
    }

    fn validate_cert(&self, status: &CertificateStatus) -> Result<CertificateStatus> {
        let mut new_status = status.clone();
        let valid_until = self.valid_until(status)?;
        let now = Utc::now();
        new_status.cert.valid = valid_until > now;
        let renew_before = self.renew_before(status)?;
        new_status.needs_renewal = renew_before <= now;
        // Reflect RenewalRequired condition
        new_status.upsert_bool(
            "RenewalRequired",
            new_status.needs_renewal,
            if new_status.needs_renewal {
                "RenewalWindow"
            } else {
                "NotInWindow"
            },
            None,
            self.metadata.generation.unwrap_or(0),
        );
        Ok(new_status)
    }

    async fn create_cert(
        &self,
        ctx: Arc<Context>,
        ns: &str,
        status: &CertificateStatus,
        api_secret: &Api<Secret>,
    ) -> Result<CertificateStatus> {
        let mut new_status = status.clone();
        let issuer_ref = &self.spec.issuer;
        let (signer_key_secret_name, signer_cert_secret_name, self_signed) =
            match issuer_ref.kind.as_str() {
                "Certificate" => {
                    let api_issuer: Api<Certificate> = Api::namespaced(
                        ctx.client.clone(),
                        issuer_ref.namespace.as_deref().unwrap_or(ns),
                    );
                    let issuer = api_issuer
                        .get_status(&issuer_ref.name)
                        .await
                        .map_err(Error::KubeError)?;
                    let issuer_status = issuer
                        .status
                        .as_ref()
                        .ok_or(Error::OtherError("Issuer status not found".to_string()))?;
                    // IssuerReady true as we could fetch usable status
                    new_status.upsert_bool(
                        "IssuerReady",
                        true,
                        "IssuerResolved",
                        Some("Issuer status available"),
                        self.metadata.generation.unwrap_or(0),
                    );
                    let key_secret_name = issuer_status
                        .key_exists
                        .then(|| issuer_status.key.secret.clone())
                        .flatten();
                    let cert_secret_name = issuer_status
                        .cert_exists
                        .then(|| issuer_status.cert.secret.clone())
                        .flatten();
                    let self_signed = issuer.metadata.uid.unwrap_or_default()
                        == self.metadata.uid.clone().unwrap_or_default();
                    (key_secret_name, cert_secret_name, self_signed)
                }
                "ExternalCertificate" => {
                    let api_issuer: Api<ExternalCertificate> = Api::namespaced(
                        ctx.client.clone(),
                        issuer_ref.namespace.as_deref().unwrap_or(ns),
                    );
                    let issuer = api_issuer
                        .get_status(&issuer_ref.name)
                        .await
                        .map_err(Error::KubeError)?;
                    let issuer_status = issuer
                        .status
                        .as_ref()
                        .ok_or(Error::OtherError("Issuer status not found".to_string()))?;
                    let key_secret_name = issuer_status
                        .key_exists
                        .then(|| issuer_status.key.secret.clone())
                        .flatten();
                    let cert_secret_name = issuer_status
                        .cert_exists
                        .then(|| issuer_status.cert.secret.clone())
                        .flatten();
                    // IssuerReady should reflect if the ExternalCertificate can actually sign (has key)
                    let issuer_ready = key_secret_name.is_some();
                    new_status.upsert_bool(
                        "IssuerReady",
                        issuer_ready,
                        if issuer_ready {
                            "IssuerResolved"
                        } else {
                            "IssuerMissingKey"
                        },
                        if issuer_ready {
                            Some("ExternalIssuer status available")
                        } else {
                            Some("ExternalCertificate issuer has no key; cannot sign")
                        },
                        self.metadata.generation.unwrap_or(0),
                    );
                    if !issuer_ready {
                        // Persist IssuerReady=false and stop issuance gracefully
                        emit_info(
                            &ctx.recorder,
                            self,
                            "IssuerNotReady",
                            "Waiting",
                            Some(format!(
                                "ExternalCertificate issuer `{}` has no key; cannot sign",
                                issuer_ref.name
                            )),
                        )
                        .await;
                        return Ok(new_status);
                    }
                    // External issuer cannot be self-signed with this Certificate CR
                    (key_secret_name, cert_secret_name, false)
                }
                _ => {
                    new_status.upsert_bool(
                        "IssuerReady",
                        false,
                        "UnsupportedIssuerKind",
                        Some(&format!("Unsupported issuer kind: {}", issuer_ref.kind)),
                        self.metadata.generation.unwrap_or(0),
                    );
                    return Err(Error::OtherError(format!(
                        "Unsupported issuer kind: {}",
                        issuer_ref.kind
                    )));
                }
            };

        let signer_key_secret_name = signer_key_secret_name
            .ok_or_else(|| Error::OtherError("Signer key secret name not found".to_string()))?;
        let signer_key_text =
            key_text_from_secret(api_secret, &signer_key_secret_name, Self::SECRET_KEY).await?;
        let my_key_secret_name = status
            .key
            .secret
            .clone()
            .ok_or(Error::OtherError("Key secret not found".to_string()))?;
        let my_key_text =
            key_text_from_secret(api_secret, &my_key_secret_name, Self::SECRET_KEY).await?;
        let std_duration: StdDuration = self
            .spec
            .renew_interval
            .unwrap_or(DurationString::new(StdDuration::from_secs(
                60 * 60 * 24 * 30,
            )))
            .into();
        let cert_info = sign_cert(
            &signer_key_text,
            &my_key_text,
            &SignCertParams {
                start: Some(Utc::now()),
                end: Some(Utc::now() + Duration::from_std(std_duration).unwrap_or_default()),
                issuer: Some(self.spec.issuer.name.clone()),
                info: None,
            },
        )?;

        let signer_cert_text = match signer_cert_secret_name {
            Some(secret_name) => {
                key_text_from_secret(api_secret, &secret_name, Self::CERT_KEY).await?
            }
            None if self_signed => cert_info.cert_text.clone(),
            None => {
                return Err(Error::OtherError(
                    "Signer cert secret not found".to_string(),
                ));
            }
        };

        let cert_secret_name = format!("{}-cert", self.name_any());
        let cert_data = self.create_owned_secret(
            cert_secret_name.clone(),
            &BTreeMap::from([
                (Self::CERT_KEY.to_string(), cert_info.cert_text.clone()),
                (Self::SIGNER_CERT_KEY.to_string(), signer_cert_text),
            ]),
        );
        let serverside = PatchParams::apply(CERTIFICATE_MANAGER_NAME);
        let cert_secret = api_secret
            .patch(&cert_secret_name, &serverside, &Patch::Apply(cert_data))
            .await
            .map_err(Error::KubeError)?;

        emit_info(
            &ctx.recorder,
            self,
            "CertCreated",
            "Created",
            Some(format!(
                "Created `{}` Cert for `{}` Certificate",
                cert_secret.name_any(),
                self.name_any()
            )),
        )
        .await;

        new_status.cert.name = Some(cert_info.name);
        new_status.cert.sig_type = Some(cert_info.sig_type);
        new_status.cert.signer_key = Some(cert_info.signer_key);
        new_status.cert.issued_at = Some(cert_info.validity.0.to_rfc3339());
        new_status.cert.valid_until = Some(cert_info.validity.1.to_rfc3339());
        new_status.cert.secret = Some(cert_secret.name_any());
        new_status.cert.valid = true;
        new_status.cert_exists = true;
        // Update availability conditions and finish issuing
        set_availability_conditions(self, &mut new_status);
        new_status.upsert_bool(
            "Issuing",
            false,
            "Renewed",
            Some("Certificate (re)issued"),
            self.metadata.generation.unwrap_or(0),
        );
        Ok(new_status)
    }

    async fn create_key(
        &self,
        ctx: Arc<Context>,
        status: &CertificateStatus,
        api_secret: &Api<Secret>,
    ) -> Result<CertificateStatus> {
        let key_info = generate_key(&self.spec.prefix)?;
        let secret_name = format!("{}-key", self.name_any());
        let secret_data = self.create_owned_secret(
            secret_name.clone(),
            &BTreeMap::from([(Self::SECRET_KEY.to_string(), key_info.key_text)]),
        );
        let secret = api_secret
            .create(&PostParams::default(), &secret_data)
            .await
            .map_err(Error::KubeError)?;
        emit_info(
            &ctx.recorder,
            self,
            "KeyCreated",
            "Created",
            Some(format!(
                "Created `{}` Key for `{}` Certificate",
                secret.name_any(),
                self.name_any()
            )),
        )
        .await;
        let mut new_status = status.clone();
        new_status.key.name = Some(key_info.name);
        new_status.key.sig_type = Some(key_info.sig_type);
        new_status.key.secret = Some(secret.name_any());
        new_status.key_exists = true;
        // KeyReady becomes true when key is generated and secret is created
        new_status.upsert_bool(
            "KeyReady",
            true,
            "KeyGenerated",
            Some("Key secret created"),
            self.metadata.generation.unwrap_or(0),
        );
        Ok(new_status)
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        emit_info(
            &ctx.recorder,
            self,
            "DeleteRequested",
            "Deleting",
            Some(format!("Delete `{}`", self.name_any())),
        )
        .await;
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

fn set_availability_conditions(cert: &Certificate, status: &mut CertificateStatus) {
    let observed_gen = cert.metadata.generation.unwrap_or(0);
    let key_ready = status.key_exists && status.key.secret.is_some();
    let cert_ready = status.cert_exists && status.cert.valid && status.cert.secret.is_some();
    status.upsert_bool(
        "KeyReady",
        key_ready,
        if key_ready {
            "KeyAvailable"
        } else {
            "KeyMissing"
        },
        None,
        observed_gen,
    );
    status.upsert_bool(
        "CertReady",
        cert_ready,
        if cert_ready {
            "CertAvailable"
        } else {
            "CertMissingOrInvalid"
        },
        None,
        observed_gen,
    );
    // Overall Ready reflects both Key and Cert readiness (independent of RenewalRequired)
    let ready = key_ready && cert_ready;
    let not_ready_msg = if ready {
        None
    } else {
        // Build a concise message about missing prerequisites
        let mut missing: Vec<&str> = Vec::new();
        if !key_ready {
            missing.push("Key");
        }
        if !cert_ready {
            missing.push("Cert");
        }
        Some(format!("Missing prerequisites: {}", missing.join(", ")))
    };
    status.upsert_bool(
        "Ready",
        ready,
        if ready {
            "Ready"
        } else {
            "PrerequisitesNotReady"
        },
        not_ready_msg.as_deref(),
        observed_gen,
    );
}

pub fn is_cert_valid() -> impl Condition<Certificate> {
    |obj: Option<&Certificate>| {
        obj.and_then(|c| c.status.clone())
            .unwrap_or_default()
            .cert
            .valid
    }
}

async fn key_text_from_secret(
    api_secret: &Api<Secret>,
    secret_name: &str,
    key: &str,
) -> Result<String> {
    use crate::helper::{Decoded, decode_secret};
    let key_secret = api_secret
        .get(secret_name)
        .await
        .map_err(Error::KubeError)?;
    let key_secret_data = decode_secret(&key_secret);
    let key_data = key_secret_data
        .get(key)
        .ok_or(Error::OtherError("Key data not found".to_string()))?;
    match key_data {
        Decoded::Utf8(s) => Ok(s.clone()),
        Decoded::Bytes(_) => Err(Error::OtherError("Key data is not UTF-8".to_string())),
    }
}

#[cfg(test)]
mod tests;

impl ExternalCertificate {
    pub const SECRET_KEY: &str = Certificate::SECRET_KEY;
    pub const CERT_KEY: &str = Certificate::CERT_KEY;

    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        debug!("Reconciling ExternalCertificate {:?}", self.name_any());
        let ns = self.namespace().unwrap();
        let api_ecert: Api<ExternalCertificate> = Api::namespaced(ctx.client.clone(), &ns);
        let api_secret: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let status = self.status().cloned().unwrap_or_default();

        // Try to load the referenced secret
        let secret = api_secret
            .get(&self.spec.secret_name)
            .await
            .map_err(Error::KubeError)?;

        // Decode secret and extract ndn.cert and optionally ndn.key
        use crate::helper::{Decoded, decode_secret};
        let data = decode_secret(&secret);
        let cert_text = match data.get(Self::CERT_KEY) {
            Some(Decoded::Utf8(s)) => Some(s.clone()),
            Some(Decoded::Bytes(_)) => None,
            None => None,
        };
        let key_text = match data.get(Self::SECRET_KEY) {
            Some(Decoded::Utf8(s)) => Some(s.clone()),
            _ => None,
        };

        let mut new_status = status.clone();
        // Populate key fields if present
        if let Some(key_text) = key_text {
            match super::crypto::KeyInfo::from_key_text(key_text) {
                Ok(info) => {
                    new_status.key.name = Some(info.name);
                    new_status.key.sig_type = Some(info.sig_type);
                    new_status.key.secret = Some(self.spec.secret_name.clone());
                    new_status.key_exists = true;
                }
                Err(e) => {
                    warn!("Failed to parse external key: {}", e);
                    new_status.key_exists = false;
                }
            }
        } else {
            new_status.key_exists = false;
        }

        // Populate cert fields
        if let Some(cert_text) = cert_text {
            match super::crypto::CertInfo::from_cert_text(cert_text) {
                Ok(info) => {
                    new_status.cert.name = Some(info.name);
                    new_status.cert.sig_type = Some(info.sig_type);
                    new_status.cert.signer_key = Some(info.signer_key);
                    new_status.cert.issued_at = Some(info.validity.0.to_rfc3339());
                    new_status.cert.valid_until = Some(info.validity.1.to_rfc3339());
                    new_status.cert.secret = Some(self.spec.secret_name.clone());
                    new_status.cert.valid = info.validity.1 > Utc::now();
                    new_status.cert_exists = true;
                }
                Err(e) => {
                    warn!("Failed to parse external cert: {}", e);
                    new_status.cert_exists = false;
                    new_status.cert.valid = false;
                }
            }
        } else {
            new_status.cert_exists = false;
            new_status.cert.valid = false;
        }

        // Update availability conditions
        set_external_availability_conditions(self, &mut new_status);

        let serverside = PatchParams::apply(EXTERNAL_CERTIFICATE_MANAGER_NAME);
        let patch = Patch::Merge(json!({ "status": new_status }));
        api_ecert
            .patch_status(self.name_any().as_str(), &serverside, &patch)
            .await
            .map_err(Error::KubeError)?;

        Ok(Action::await_change())
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        emit_info(
            &ctx.recorder,
            self,
            "DeleteRequested",
            "Deleting",
            Some(format!("Delete `{}`", self.name_any())),
        )
        .await;
        Ok(Action::await_change())
    }
}

fn set_external_availability_conditions(
    ecert: &ExternalCertificate,
    status: &mut ExternalCertificateStatus,
) {
    let observed_gen = ecert.metadata.generation.unwrap_or(0);
    let key_ready = status.key_exists && status.key.secret.is_some();
    let cert_ready = status.cert_exists && status.cert.valid && status.cert.secret.is_some();
    status.upsert_bool(
        "KeyReady",
        key_ready,
        if key_ready {
            "KeyAvailable"
        } else {
            "KeyMissing"
        },
        None,
        observed_gen,
    );
    status.upsert_bool(
        "CertReady",
        cert_ready,
        if cert_ready {
            "CertAvailable"
        } else {
            "CertMissingOrInvalid"
        },
        None,
        observed_gen,
    );
    // ExternalCertificate is considered Ready if the certificate is present and valid,
    // even when the key is absent (it can still be used as a trust anchor).
    let ready = cert_ready;
    status.upsert_bool(
        "Ready",
        ready,
        if ready {
            "Ready"
        } else {
            "PrerequisitesNotReady"
        },
        None,
        observed_gen,
    );
}

pub fn is_external_cert_valid() -> impl Condition<ExternalCertificate> {
    |obj: Option<&ExternalCertificate>| {
        obj.and_then(|c| c.status.clone())
            .unwrap_or_default()
            .cert
            .valid
    }
}

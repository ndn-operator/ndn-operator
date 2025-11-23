use crate::{
    conditions::Conditions,
    Error, Result,
    cert_controller::{ExternalCertificate, ExternalCertificateStatus},
    events_helper::emit_info,
};
use chrono::Utc;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, Patch, PatchParams, ResourceExt};
use kube::core::object::HasStatus;
use kube::runtime::{controller::Action, wait::Condition};
use serde_json::json;
use std::sync::Arc;
use tracing::*;

use super::Context;
use crate::cert_controller::{CertInfo, KeyInfo};

pub static EXTERNAL_CERTIFICATE_MANAGER_NAME: &str = "external-cert-controller";

impl ExternalCertificate {
    pub const SECRET_KEY: &str = crate::cert_controller::Certificate::SECRET_KEY;
    pub const CERT_KEY: &str = crate::cert_controller::Certificate::CERT_KEY;

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
            match KeyInfo::from_key_text(key_text) {
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
            match CertInfo::from_cert_text(cert_text) {
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

use chrono::{DateTime, Utc};
use std::{env, io::Write, path::Path, process::Command};
use tempfile::NamedTempFile;
use tracing::*;

use crate::{Error, Result};

pub struct KeyInfo {
    pub name: String,
    pub sig_type: String,
    pub key_text: String,
}

impl KeyInfo {
    pub(crate) fn from_key_text(key_text: String) -> Result<Self, Error> {
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
            sig_type: sig_type
                .ok_or_else(|| Error::OtherError("Could not parse sig type".to_string()))?,
            key_text,
        })
    }
}

#[derive(Default)]
pub struct SignCertParams {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub issuer: Option<String>,
    pub info: Option<String>,
}

pub struct CertInfo {
    pub name: String,
    pub sig_type: String,
    pub signer_key: String,
    pub validity: (DateTime<Utc>, DateTime<Utc>),
    pub cert_text: String,
}

impl CertInfo {
    pub(crate) fn from_cert_text(cert_text: String) -> Result<Self, Error> {
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
                    .map_err(|e| Error::OtherError(format!("Failed to parse start date: {e}")))?
                    .with_timezone(&Utc);
                let end = DateTime::parse_from_str(parts[1], "%Y-%m-%d %H:%M:%S %z %Z")
                    .map_err(|e| Error::OtherError(format!("Failed to parse end date: {e}")))?
                    .with_timezone(&Utc);
                validity = (start, end);
            }
        }
        Ok(CertInfo {
            name: name.ok_or_else(|| Error::OtherError("Could not parse cert name".to_string()))?,
            sig_type: sig_type
                .ok_or_else(|| Error::OtherError("Could not parse sig type".to_string()))?,
            signer_key: signer_key
                .ok_or_else(|| Error::OtherError("Could not parse signer key".to_string()))?,
            validity,
            cert_text,
        })
    }
}

pub fn generate_key(prefix: &str) -> Result<KeyInfo, Error> {
    let ndnd_path: &str = option_env!("NDND_PATH").unwrap_or("/ndnd");
    let output = Command::new(ndnd_path)
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
    let key_text =
        String::from_utf8(output.stdout).map_err(|e| Error::OtherError(e.to_string()))?;
    KeyInfo::from_key_text(key_text)
}

pub fn sign_cert(
    signer_key: &str,
    cert_key: &str,
    params: &SignCertParams,
) -> Result<CertInfo, Error> {
    let ndnd_path: &str = option_env!("NDND_PATH").unwrap_or("/ndnd");
    let binding = env::var("TMP_DIR").unwrap_or("/tmp".to_string());
    let tmp_dir = Path::new(&binding);
    let mut temp_signer_key_file = NamedTempFile::new_in(tmp_dir).map_err(Error::IoError)?;
    debug!(
        "Temporary signer key file created: {:?}",
        temp_signer_key_file.path()
    );
    writeln!(temp_signer_key_file, "{signer_key}").map_err(Error::IoError)?;
    let mut temp_cert_key_file = NamedTempFile::new_in(tmp_dir).map_err(Error::IoError)?;
    debug!(
        "Temporary cert key file created: {:?}",
        temp_cert_key_file.path()
    );
    writeln!(temp_cert_key_file, "{cert_key}").map_err(Error::IoError)?;

    let mut args = vec![
        "sec",
        "sign-cert",
        temp_signer_key_file.path().to_str().unwrap(),
    ];
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
    let output = Command::new(ndnd_path)
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
    let cert_text =
        String::from_utf8(output.stdout).map_err(|e| Error::OtherError(e.to_string()))?;
    CertInfo::from_cert_text(cert_text)
}

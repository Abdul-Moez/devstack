//! A local certificate authority plus one certificate per site, so https://name.test works
//! without warnings once the CA is trusted (Windows asks you to confirm once).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SerialNumber,
};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::paths::Paths;
use crate::util::{command, hex, now_secs, run, IS_WIN};

const CA_DAYS: i64 = 3650;
const CERT_DAYS: i64 = 397; // browsers reject leaf certs valid for more than 398 days
const RENEW_BEFORE_SECS: u64 = 30 * 86400;

pub fn ca_files(paths: &Paths) -> (PathBuf, PathBuf) {
    let d = paths.ssl.join("ca");
    (d.join("devstack-ca.crt"), d.join("devstack-ca.key"))
}

fn serial() -> SerialNumber {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut bytes = nanos.to_be_bytes().to_vec();
    bytes[0] &= 0x7f; // positive
    SerialNumber::from_slice(&bytes)
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this computer".into())
}

/// Creates the CA once; returns (cert PEM, key PEM).
pub fn ensure_ca(paths: &Paths) -> Result<(String, String)> {
    let (crt, key) = ca_files(paths);
    if crt.exists() && key.exists() {
        return Ok((fs::read_to_string(&crt)?, fs::read_to_string(&key)?));
    }
    let kp = KeyPair::generate()?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("devstack local CA ({})", hostname()));
    dn.push(DnType::OrganizationName, "devstack");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    params.serial_number = Some(serial());
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(CA_DAYS);
    let cert = params.self_signed(&kp)?;
    fs::create_dir_all(crt.parent().unwrap())?;
    fs::write(&key, kp.serialize_pem())?;
    fs::write(&crt, cert.pem())?;
    Ok((cert.pem(), kp.serialize_pem()))
}

fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    Ok(base64::engine::general_purpose::STANDARD.decode(b64.trim())?)
}

pub fn thumbprint(paths: &Paths) -> Result<String> {
    let (pem, _) = ensure_ca(paths)?;
    Ok(hex(&Sha1::digest(pem_to_der(&pem)?)).to_uppercase())
}

/// Returns (cert path, key path, regenerated?).
pub fn ensure_cert(paths: &Paths, name: &str, hosts: &[String]) -> Result<(PathBuf, PathBuf, bool)> {
    let (ca_pem, ca_key_pem) = ensure_ca(paths)?;
    let dir = paths.ssl.join("sites");
    let crt = dir.join(format!("{name}.crt"));
    let key = dir.join(format!("{name}.key"));
    let meta_path = dir.join(format!("{name}.json"));
    let mut wanted: Vec<String> = hosts.to_vec();
    wanted.sort();
    wanted.dedup();
    let ca_thumb = thumbprint(paths)?;
    if crt.exists() && key.exists() {
        if let Some(meta) = crate::util::load_json(&meta_path) {
            let same_hosts = meta.get("hosts") == Some(&json!(wanted));
            let same_ca = meta.get("ca").and_then(Value::as_str) == Some(ca_thumb.as_str());
            let fresh = meta.get("expires").and_then(Value::as_u64).unwrap_or(0) > now_secs() + RENEW_BEFORE_SECS;
            if same_hosts && same_ca && fresh {
                return Ok((crt, key, false));
            }
        }
    }
    let ca_key = KeyPair::from_pem(&ca_key_pem)?;
    let ca_cert = CertificateParams::from_ca_cert_pem(&ca_pem)?.self_signed(&ca_key)?;
    let kp = KeyPair::generate()?;
    let mut params = CertificateParams::new(wanted.clone())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, wanted.iter().find(|h| !h.starts_with('*')).cloned().unwrap_or_else(|| wanted[0].clone()));
    params.distinguished_name = dn;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    params.serial_number = Some(serial());
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(CERT_DAYS);
    let cert = params.signed_by(&kp, &ca_cert, &ca_key)?;
    fs::create_dir_all(&dir)?;
    fs::write(&key, kp.serialize_pem())?;
    fs::write(&crt, cert.pem())?;
    let expires = now_secs() + CERT_DAYS as u64 * 86400;
    fs::write(&meta_path, serde_json::to_string_pretty(&json!({"hosts": wanted, "ca": ca_thumb, "expires": expires}))?)?;
    Ok((crt, key, true))
}

pub fn is_trusted(paths: &Paths) -> Result<bool> {
    if !IS_WIN {
        return Ok(false);
    }
    Ok(run(command("certutil", ["-user", "-store", "Root", &thumbprint(paths)?]), Duration::from_secs(30))?.ok())
}

/// Adds the CA to the current user's trusted roots. Windows shows a confirmation dialog.
pub fn trust(paths: &Paths) -> Result<bool> {
    if !IS_WIN {
        bail!("Trusting the CA is only automated on Windows.");
    }
    ensure_ca(paths)?;
    let (crt, _) = ca_files(paths);
    let out = run(command("certutil", ["-user".to_string(), "-addstore".into(), "Root".into(), crt.display().to_string()]), Duration::from_secs(600))
        .context("running certutil")?;
    Ok(out.ok())
}

pub fn untrust(paths: &Paths) -> Result<bool> {
    if !IS_WIN {
        return Ok(false);
    }
    Ok(run(command("certutil", ["-user", "-delstore", "Root", &thumbprint(paths)?]), Duration::from_secs(600))?.ok())
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}

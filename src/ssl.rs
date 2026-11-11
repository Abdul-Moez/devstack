//! A local certificate authority plus one certificate per site, so https://name.test works
//! without warnings once the CA is trusted (Windows asks you to confirm once).
//!
//! The CA carries X.509 name constraints: it can only vouch for the local zones devstack
//! uses (.test, localhost, your own host names) and for 127.0.0.1. Its private key sits
//! unencrypted in etc/ssl/ca/; if that file is ever copied, it still can't be used to
//! impersonate real websites to this computer.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType, ExtendedKeyUsagePurpose, GeneralSubtree, IsCa,
    KeyPair, KeyUsagePurpose, NameConstraints, SerialNumber,
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

/// Top-level names that only exist for local use: one zone covers every site under them.
const LOCAL_TLDS: &[&str] = &["test", "localhost", "local", "example", "invalid", "internal", "lan", "home", "corp"];

/// The DNS zone the CA must be allowed to sign for, for one host name. `suffix` is the fixed
/// end of the host name template ("test" for "{name}.test", "dev.acme.com" for
/// "{name}.dev.acme.com").
pub fn zone_for(host: &str, suffix: &str) -> String {
    let host = host.trim_start_matches("*.").to_lowercase();
    let tld = host.rsplit('.').next().unwrap_or("");
    if LOCAL_TLDS.contains(&tld) {
        tld.to_string()
    } else if !suffix.is_empty() && (host == suffix || host.ends_with(&format!(".{suffix}"))) {
        suffix.to_string()
    } else {
        host
    }
}

fn ca_meta(paths: &Paths) -> PathBuf {
    paths.ssl.join("ca").join("devstack-ca.json")
}

/// Zones the current CA may sign for (None for a CA made before it had constraints).
pub fn ca_zones(paths: &Paths) -> Option<Vec<String>> {
    let v = crate::util::load_json(&ca_meta(paths))?;
    Some(v["zones"].as_array()?.iter().filter_map(|z| z.as_str().map(str::to_string)).collect())
}

fn create_ca(paths: &Paths, zones: &[String]) -> Result<(String, String)> {
    let (crt, key) = ca_files(paths);
    let kp = KeyPair::generate()?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("devstack local CA ({})", hostname()));
    dn.push(DnType::OrganizationName, "devstack");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    // Only these zones, and only the loopback addresses.
    let mut permitted: Vec<GeneralSubtree> = zones.iter().map(|z| GeneralSubtree::DnsName(z.clone())).collect();
    permitted.push(GeneralSubtree::IpAddress(CidrSubnet::V4([127, 0, 0, 0], [255, 0, 0, 0])));
    let mut loopback6 = [0u8; 16];
    loopback6[15] = 1;
    permitted.push(GeneralSubtree::IpAddress(CidrSubnet::V6(loopback6, [0xff; 16])));
    params.name_constraints = Some(NameConstraints { permitted_subtrees: permitted, excluded_subtrees: vec![] });
    params.serial_number = Some(serial());
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(CA_DAYS);
    let cert = params.self_signed(&kp)?;
    fs::create_dir_all(crt.parent().unwrap())?;
    fs::write(&key, kp.serialize_pem())?;
    fs::write(&crt, cert.pem())?;
    fs::write(ca_meta(paths), serde_json::to_string_pretty(&json!({"zones": zones}))?)?;
    Ok((cert.pem(), kp.serialize_pem()))
}

/// Creates the CA once (for .test and localhost); returns (cert PEM, key PEM).
pub fn ensure_ca(paths: &Paths) -> Result<(String, String)> {
    let (crt, key) = ca_files(paths);
    if crt.exists() && key.exists() {
        return Ok((fs::read_to_string(&crt)?, fs::read_to_string(&key)?));
    }
    create_ca(paths, &["localhost".to_string(), "test".to_string()])
}

/// Make sure the CA may sign for every one of `hosts`. Returns true if the CA had to be made
/// again (a host in a zone it didn't cover, or a CA from before constraints existed): the new
/// one has to be trusted again. The old key is overwritten, so the old CA is dead even if
/// Windows still lists it.
pub fn ensure_ca_covers(paths: &Paths, hosts: &[String], suffix: &str) -> Result<bool> {
    let mut zones: Vec<String> = hosts.iter().map(|h| zone_for(h, suffix)).collect();
    zones.extend(["localhost".to_string(), "test".to_string()]);
    zones.sort();
    zones.dedup();
    let (crt, key) = ca_files(paths);
    let existed = crt.exists() && key.exists();
    if existed {
        if let Some(have) = ca_zones(paths) {
            if zones.iter().all(|z| have.contains(z)) {
                return Ok(false);
            }
            zones.extend(have);
            zones.sort();
            zones.dedup();
        }
    }
    create_ca(paths, &zones)?;
    Ok(existed)
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

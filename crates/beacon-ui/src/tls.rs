//! Read-only certificate details derived from a TLS Secret's `tls.crt`.
//! The private key is never parsed or rendered here.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use beacon_kube::DynamicObject;
use sha2::{Digest, Sha256};
use x509_parser::{
    certificate::X509Certificate,
    extensions::{DistributionPointName, GeneralName, ParsedExtension},
    pem::Pem,
    public_key::PublicKey,
};

#[derive(Debug)]
pub struct CertificateInfo {
    pub subject: String,
    pub common_name: Option<String>,
    pub issuer: String,
    pub serial: String,
    pub version: u32,
    pub not_before: String,
    pub not_after: String,
    pub not_before_unix: i64,
    pub not_after_unix: i64,
    pub signature_algorithm: String,
    pub public_key_algorithm: String,
    pub public_key_bits: Option<usize>,
    pub public_key_details: Option<String>,
    pub public_key_pem: String,
    pub sha256_fingerprint: String,
    pub extensions: Vec<ExtensionInfo>,
}

#[derive(Debug)]
pub struct ExtensionInfo {
    pub name: String,
    pub critical: bool,
    pub details: String,
}

impl CertificateInfo {
    pub fn validity_at(&self, now: i64) -> String {
        if now < self.not_before_unix {
            "Not yet valid".into()
        } else if now >= self.not_after_unix {
            "Expired".into()
        } else {
            let days = (self.not_after_unix - now + 86_399) / 86_400;
            format!("Not expired · {days} days remaining")
        }
    }
}

/// A Secret may carry a leaf certificate followed by intermediates. Keep that
/// order, and make a malformed or missing certificate visible in the details.
pub fn inspect(object: &DynamicObject) -> Result<Vec<CertificateInfo>, String> {
    let encoded = object
        .data
        .get("data")
        .and_then(|data| data.get("tls.crt"))
        .and_then(serde_json::Value::as_str)
        .ok_or("This TLS Secret has no tls.crt entry")?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| format!("tls.crt is not valid base64: {error}"))?;

    let mut certificates = Vec::new();
    for block in Pem::iter_from_buffer(&bytes) {
        let block = block.map_err(|error| format!("Cannot read tls.crt PEM: {error}"))?;
        if block.label != "CERTIFICATE" {
            continue;
        }
        let certificate = block
            .parse_x509()
            .map_err(|error| format!("Cannot parse a certificate in tls.crt: {error}"))?;
        certificates.push(describe(&certificate, &block.contents));
    }

    if certificates.is_empty() {
        return Err("tls.crt contains no PEM certificate".into());
    }
    Ok(certificates)
}

fn describe(cert: &X509Certificate<'_>, der: &[u8]) -> CertificateInfo {
    let public_key = cert.public_key();
    let parsed_key = public_key.parsed().ok();
    let public_key_bits = parsed_key
        .as_ref()
        .map(PublicKey::key_size)
        .filter(|&bits| bits > 0);
    let public_key_details = match &parsed_key {
        Some(PublicKey::RSA(key)) => key
            .try_exponent()
            .ok()
            .map(|exponent| format!("RSA exponent: {exponent}")),
        Some(PublicKey::EC(_)) => public_key
            .algorithm
            .parameters
            .as_ref()
            .and_then(|parameters| parameters.as_oid().ok())
            .map(|curve| {
                let oid = curve.to_string();
                let name = match oid.as_str() {
                    "1.2.840.10045.3.1.7" => "P-256",
                    "1.3.132.0.34" => "P-384",
                    "1.3.132.0.35" => "P-521",
                    "1.3.132.0.10" => "secp256k1",
                    _ => "Unknown curve",
                };
                format!("Curve: {name} ({oid})")
            }),
        _ => None,
    };

    CertificateInfo {
        subject: cert.subject().to_string(),
        common_name: cert
            .subject()
            .iter_common_name()
            .next()
            .and_then(|name| name.as_str().ok())
            .map(str::to_owned),
        issuer: cert.issuer().to_string(),
        serial: cert.raw_serial_as_string(),
        version: cert.version().0 + 1,
        not_before: cert.validity().not_before.to_string(),
        not_after: cert.validity().not_after.to_string(),
        not_before_unix: cert.validity().not_before.timestamp(),
        not_after_unix: cert.validity().not_after.timestamp(),
        signature_algorithm: algorithm_name(&cert.signature_algorithm.algorithm.to_string()),
        public_key_algorithm: algorithm_name(&public_key.algorithm.algorithm.to_string()),
        public_key_bits,
        public_key_details,
        public_key_pem: pem_public_key(public_key.raw),
        sha256_fingerprint: hex(&Sha256::digest(der)),
        extensions: cert.iter_extensions().map(extension).collect(),
    }
}

fn extension(ext: &x509_parser::extensions::X509Extension<'_>) -> ExtensionInfo {
    let oid = ext.oid.to_string();
    let (name, details) = match ext.parsed_extension() {
        ParsedExtension::SubjectAlternativeName(names) => (
            "Subject Alternative Name".to_string(),
            names
                .general_names
                .iter()
                .map(general_name)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ParsedExtension::KeyUsage(usage) => ("Key Usage".into(), usage.to_string()),
        ParsedExtension::ExtendedKeyUsage(usage) => {
            let mut names = Vec::new();
            for (enabled, name) in [
                (usage.any, "Any"),
                (usage.server_auth, "TLS server authentication"),
                (usage.client_auth, "TLS client authentication"),
                (usage.code_signing, "Code signing"),
                (usage.email_protection, "Email protection"),
                (usage.time_stamping, "Time stamping"),
                (usage.ocsp_signing, "OCSP signing"),
            ] {
                if enabled {
                    names.push(name.to_string());
                }
            }
            names.extend(usage.other.iter().map(ToString::to_string));
            ("Extended Key Usage".into(), names.join(", "))
        }
        ParsedExtension::BasicConstraints(constraints) => (
            "Basic Constraints".into(),
            match constraints.path_len_constraint {
                Some(length) => format!("CA: {}; path length: {length}", constraints.ca),
                None => format!("CA: {}", constraints.ca),
            },
        ),
        ParsedExtension::SubjectKeyIdentifier(key) => ("Subject Key Identifier".into(), hex(key.0)),
        ParsedExtension::AuthorityKeyIdentifier(key) => (
            "Authority Key Identifier".into(),
            key.key_identifier
                .as_ref()
                .map(|identifier| hex(identifier.0))
                .unwrap_or_else(|| "No key identifier".into()),
        ),
        ParsedExtension::CertificatePolicies(policies) => (
            "Certificate Policies".into(),
            policies
                .iter()
                .map(|policy| policy.policy_id.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ParsedExtension::IssuerAlternativeName(names) => (
            "Issuer Alternative Name".into(),
            names
                .general_names
                .iter()
                .map(general_name)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ParsedExtension::AuthorityInfoAccess(access) => (
            "Authority Information Access".into(),
            access
                .iter()
                .map(|entry| {
                    format!(
                        "{}: {}",
                        entry.access_method,
                        general_name(&entry.access_location)
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ParsedExtension::CRLDistributionPoints(points) => (
            "CRL Distribution Points".into(),
            points
                .iter()
                .filter_map(|point| match &point.distribution_point {
                    Some(DistributionPointName::FullName(names)) => Some(
                        names
                            .iter()
                            .map(general_name)
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                    Some(DistributionPointName::NameRelativeToCRLIssuer(name)) => {
                        Some(format!("Relative name: {name:?}"))
                    }
                    None => None,
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => (
            oid.clone(),
            format!("{} DER bytes: {}", ext.value.len(), hex_preview(ext.value)),
        ),
    };

    ExtensionInfo {
        name: format!("{name} ({oid})"),
        critical: ext.critical,
        details,
    }
}

fn general_name(name: &GeneralName<'_>) -> String {
    match name {
        GeneralName::DNSName(value) => format!("DNS: {value}"),
        GeneralName::IPAddress(value) => match value.len() {
            4 => format!("IP: {}.{}.{}.{}", value[0], value[1], value[2], value[3]),
            16 => {
                let mut octets = [0; 16];
                octets.copy_from_slice(value);
                format!("IP: {}", std::net::Ipv6Addr::from(octets))
            }
            _ => format!("IP: {}", hex(value)),
        },
        GeneralName::RFC822Name(value) => format!("Email: {value}"),
        GeneralName::URI(value) => format!("URI: {value}"),
        GeneralName::DirectoryName(value) => format!("Directory: {value}"),
        GeneralName::RegisteredID(value) => format!("Registered ID: {value}"),
        _ => format!("{name:?}"),
    }
}

fn algorithm_name(oid: &str) -> String {
    let name = match oid {
        "1.2.840.113549.1.1.1" => "RSA",
        "1.2.840.10045.2.1" => "Elliptic curve",
        "1.3.101.112" => "Ed25519",
        "1.3.101.113" => "Ed448",
        "1.2.840.113549.1.1.5" => "SHA-1 with RSA",
        "1.2.840.113549.1.1.10" => "RSA-PSS",
        "1.2.840.113549.1.1.11" => "SHA-256 with RSA",
        "1.2.840.113549.1.1.12" => "SHA-384 with RSA",
        "1.2.840.113549.1.1.13" => "SHA-512 with RSA",
        "1.2.840.10045.4.3.2" => "ECDSA with SHA-256",
        "1.2.840.10045.4.3.3" => "ECDSA with SHA-384",
        "1.2.840.10045.4.3.4" => "ECDSA with SHA-512",
        _ => return oid.to_string(),
    };
    format!("{name} ({oid})")
}

fn pem_public_key(der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN PUBLIC KEY-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END PUBLIC KEY-----");
    pem
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn hex_preview(bytes: &[u8]) -> String {
    let preview = hex(&bytes[..bytes.len().min(32)]);
    if bytes.len() > 32 {
        format!("{preview}…")
    } else {
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beacon_kube::resources;

    fn secret(crt: Option<&str>) -> DynamicObject {
        let mut resource = resources::pod();
        resource.kind = "Secret".into();
        resource.plural = "secrets".into();
        let mut data = serde_json::json!({ "type": "kubernetes.io/tls", "data": {} });
        if let Some(crt) = crt {
            data["data"]["tls.crt"] = STANDARD.encode(crt).into();
        }
        DynamicObject::new("website", &resource)
            .within("default")
            .data(data)
    }

    #[test]
    fn reads_identity_validity_public_key_and_extensions() {
        let object = secret(Some(include_str!("testdata/tls.crt")));
        let certs = inspect(&object).unwrap();
        assert_eq!(certs.len(), 1);
        let cert = &certs[0];
        assert_eq!(cert.common_name.as_deref(), Some("example.test"));
        assert!(cert.subject.contains("Beacon QA"));
        assert!(cert.issuer.contains("example.test"));
        assert_eq!(cert.public_key_bits, Some(256));
        assert!(
            cert.public_key_details
                .as_deref()
                .unwrap()
                .contains("P-256")
        );
        assert!(cert.signature_algorithm.contains("ECDSA with SHA-256"));
        assert!(
            cert.public_key_pem
                .starts_with("-----BEGIN PUBLIC KEY-----")
        );
        assert_eq!(cert.sha256_fingerprint.split(':').count(), 32);
        assert!(cert.extensions.iter().any(|ext| {
            ext.name.contains("Subject Alternative Name")
                && ext.details.contains("DNS: example.test")
        }));
        assert!(
            cert.extensions
                .iter()
                .any(|ext| { ext.name.contains("Key Usage") && ext.critical })
        );
        assert_eq!(cert.validity_at(cert.not_before_unix - 1), "Not yet valid");
        assert_eq!(cert.validity_at(cert.not_after_unix), "Expired");
        assert!(
            cert.validity_at(cert.not_before_unix)
                .starts_with("Not expired")
        );
    }

    #[test]
    fn reports_missing_or_invalid_certificate() {
        assert!(inspect(&secret(None)).unwrap_err().contains("no tls.crt"));
        assert!(
            inspect(&secret(Some("not PEM")))
                .unwrap_err()
                .contains("no PEM certificate")
        );
    }

    #[test]
    fn reads_every_certificate_in_a_chain() {
        let pem = include_str!("testdata/tls.crt");
        let object = secret(Some(&format!("{pem}\n{pem}")));
        assert_eq!(inspect(&object).unwrap().len(), 2);
    }
}

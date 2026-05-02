//! Custom CA handling for Savfox outbound HTTP clients.
//!
//! Enterprise TLS-inspecting proxies often require clients to trust a local root
//! CA bundle. This module centralizes the process-wide environment policy for
//! reqwest clients so login, model, update, and registry requests can all share
//! the same behavior.

use std::env;
use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

use rustls_pemfile::Item;
use thiserror::Error;
use tracing::{info, warn};

pub const SAVFOX_CA_CERT_ENV: &str = "SAVFOX_CA_CERTIFICATE";
pub const CODEX_CA_CERT_ENV: &str = "CODEX_CA_CERTIFICATE";
pub const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";

const CA_CERT_HINT: &str = "If you set SAVFOX_CA_CERTIFICATE, CODEX_CA_CERTIFICATE, or SSL_CERT_FILE, ensure it points to a PEM file containing one or more CERTIFICATE blocks, or unset it to use system roots.";

#[derive(Debug, Error)]
pub enum BuildCustomCaTransportError {
    #[error(
        "Failed to read CA certificate file {} selected by {}: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    ReadCaFile {
        source_env: &'static str,
        path: PathBuf,
        source: io::Error,
    },

    #[error(
        "Failed to load CA certificates from {} selected by {}: {detail}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    InvalidCaFile {
        source_env: &'static str,
        path: PathBuf,
        detail: String,
    },

    #[error(
        "Failed to parse certificate #{certificate_index} from {} selected by {}: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    RegisterCertificate {
        source_env: &'static str,
        path: PathBuf,
        certificate_index: usize,
        source: reqwest::Error,
    },

    #[error(
        "Failed to build HTTP client while using CA bundle from {} ({}): {source}",
        source_env,
        path.display()
    )]
    BuildClientWithCustomCa {
        source_env: &'static str,
        path: PathBuf,
        #[source]
        source: reqwest::Error,
    },

    #[error("Failed to build HTTP client while using system root certificates: {0}")]
    BuildClientWithSystemRoots(#[source] reqwest::Error),
}

impl From<BuildCustomCaTransportError> for io::Error {
    fn from(error: BuildCustomCaTransportError) -> Self {
        match error {
            BuildCustomCaTransportError::ReadCaFile { ref source, .. } => {
                io::Error::new(source.kind(), error)
            }
            BuildCustomCaTransportError::InvalidCaFile { .. }
            | BuildCustomCaTransportError::RegisterCertificate { .. } => {
                io::Error::new(io::ErrorKind::InvalidData, error)
            }
            BuildCustomCaTransportError::BuildClientWithCustomCa { .. }
            | BuildCustomCaTransportError::BuildClientWithSystemRoots(_) => io::Error::other(error),
        }
    }
}

pub fn build_reqwest_client_with_custom_ca(
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, BuildCustomCaTransportError> {
    build_reqwest_client_with_env(&ProcessEnv, builder)
}

fn build_reqwest_client_with_env(
    env_source: &dyn EnvSource,
    mut builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, BuildCustomCaTransportError> {
    let Some(bundle) = env_source.configured_ca_bundle() else {
        return builder
            .build()
            .map_err(BuildCustomCaTransportError::BuildClientWithSystemRoots);
    };

    info!(
        source_env = bundle.source_env,
        ca_path = %bundle.path.display(),
        "building HTTP client with rustls backend for custom CA bundle"
    );
    builder = builder.use_rustls_tls();

    let certificates = bundle.load_certificates()?;
    for (idx, cert_der) in certificates.iter().enumerate() {
        let certificate = reqwest::Certificate::from_der(cert_der).map_err(|source| {
            warn!(
                source_env = bundle.source_env,
                ca_path = %bundle.path.display(),
                certificate_index = idx + 1,
                error = %source,
                "failed to register CA certificate"
            );
            BuildCustomCaTransportError::RegisterCertificate {
                source_env: bundle.source_env,
                path: bundle.path.clone(),
                certificate_index: idx + 1,
                source,
            }
        })?;
        builder = builder.add_root_certificate(certificate);
    }

    builder.build().map_err(
        |source| BuildCustomCaTransportError::BuildClientWithCustomCa {
            source_env: bundle.source_env,
            path: bundle.path.clone(),
            source,
        },
    )
}

trait EnvSource {
    fn var(&self, key: &str) -> Option<String>;

    fn non_empty_path(&self, key: &str) -> Option<PathBuf> {
        self.var(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn configured_ca_bundle(&self) -> Option<ConfiguredCaBundle> {
        self.non_empty_path(SAVFOX_CA_CERT_ENV)
            .map(|path| ConfiguredCaBundle {
                source_env: SAVFOX_CA_CERT_ENV,
                path,
            })
            .or_else(|| {
                self.non_empty_path(CODEX_CA_CERT_ENV)
                    .map(|path| ConfiguredCaBundle {
                        source_env: CODEX_CA_CERT_ENV,
                        path,
                    })
            })
            .or_else(|| {
                self.non_empty_path(SSL_CERT_FILE_ENV)
                    .map(|path| ConfiguredCaBundle {
                        source_env: SSL_CERT_FILE_ENV,
                        path,
                    })
            })
    }
}

struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

struct ConfiguredCaBundle {
    source_env: &'static str,
    path: PathBuf,
}

impl ConfiguredCaBundle {
    fn load_certificates(&self) -> Result<Vec<Vec<u8>>, BuildCustomCaTransportError> {
        let pem_data =
            fs::read(&self.path).map_err(|source| BuildCustomCaTransportError::ReadCaFile {
                source_env: self.source_env,
                path: self.path.clone(),
                source,
            })?;

        let normalized_pem = NormalizedPem::from_pem_data(self.source_env, &self.path, &pem_data);
        let mut certificates = Vec::new();
        let mut ignored_crl = false;
        let mut cursor = Cursor::new(normalized_pem.contents().as_bytes());
        for item in rustls_pemfile::read_all(&mut cursor) {
            let item = item.map_err(|error| {
                self.invalid_ca_file(format!("failed to parse PEM file: {error}"))
            })?;
            match item {
                Item::X509Certificate(cert) => {
                    let der = normalized_pem.certificate_der(cert.as_ref()).ok_or_else(|| {
                        self.invalid_ca_file(
                            "failed to extract certificate data from TRUSTED CERTIFICATE: invalid DER length",
                        )
                    })?;
                    certificates.push(der.to_vec());
                }
                Item::Crl(_) => {
                    if !ignored_crl {
                        info!(
                            source_env = self.source_env,
                            ca_path = %self.path.display(),
                            "ignoring X509 CRL entries found in custom CA bundle"
                        );
                        ignored_crl = true;
                    }
                }
                _ => {}
            }
        }

        if certificates.is_empty() {
            return Err(self.invalid_ca_file("no certificates found in PEM file"));
        }

        info!(
            source_env = self.source_env,
            ca_path = %self.path.display(),
            certificate_count = certificates.len(),
            "loaded certificates from custom CA bundle"
        );
        Ok(certificates)
    }

    fn invalid_ca_file(&self, detail: impl std::fmt::Display) -> BuildCustomCaTransportError {
        BuildCustomCaTransportError::InvalidCaFile {
            source_env: self.source_env,
            path: self.path.clone(),
            detail: detail.to_string(),
        }
    }
}

enum NormalizedPem {
    Standard(String),
    TrustedCertificate(String),
}

impl NormalizedPem {
    fn from_pem_data(source_env: &'static str, path: &Path, pem_data: &[u8]) -> Self {
        let pem = String::from_utf8_lossy(pem_data);
        if pem.contains("TRUSTED CERTIFICATE") {
            info!(
                source_env,
                ca_path = %path.display(),
                "normalizing OpenSSL TRUSTED CERTIFICATE labels in custom CA bundle"
            );
            Self::TrustedCertificate(
                pem.replace("BEGIN TRUSTED CERTIFICATE", "BEGIN CERTIFICATE")
                    .replace("END TRUSTED CERTIFICATE", "END CERTIFICATE"),
            )
        } else {
            Self::Standard(pem.into_owned())
        }
    }

    fn contents(&self) -> &str {
        match self {
            Self::Standard(contents) | Self::TrustedCertificate(contents) => contents,
        }
    }

    fn certificate_der<'a>(&self, der: &'a [u8]) -> Option<&'a [u8]> {
        match self {
            Self::Standard(_) => Some(der),
            Self::TrustedCertificate(_) => first_der_item(der),
        }
    }
}

fn first_der_item(der: &[u8]) -> Option<&[u8]> {
    der_item_length(der).map(|length| &der[..length])
}

fn der_item_length(der: &[u8]) -> Option<usize> {
    let &length_octet = der.get(1)?;
    if length_octet & 0x80 == 0 {
        return Some(2 + usize::from(length_octet)).filter(|length| *length <= der.len());
    }

    let length_octets = usize::from(length_octet & 0x7f);
    if length_octets == 0 {
        return None;
    }

    let length_start = 2usize;
    let length_end = length_start.checked_add(length_octets)?;
    let length_bytes = der.get(length_start..length_end)?;
    let mut content_length = 0usize;
    for &byte in length_bytes {
        content_length = content_length
            .checked_mul(256)?
            .checked_add(usize::from(byte))?;
    }

    length_end
        .checked_add(content_length)
        .filter(|length| *length <= der.len())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    struct MapEnv {
        values: HashMap<String, String>,
    }

    impl EnvSource for MapEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }
    }

    fn map_env(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv {
            values: pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn ca_path_prefers_savfox_env() {
        let env = map_env(&[
            (SAVFOX_CA_CERT_ENV, "/tmp/savfox.pem"),
            (CODEX_CA_CERT_ENV, "/tmp/codex.pem"),
            (SSL_CERT_FILE_ENV, "/tmp/fallback.pem"),
        ]);

        assert_eq!(
            env.configured_ca_bundle().map(|bundle| bundle.path),
            Some(PathBuf::from("/tmp/savfox.pem"))
        );
    }

    #[test]
    fn ca_path_accepts_codex_env_for_compatibility() {
        let env = map_env(&[
            (SAVFOX_CA_CERT_ENV, ""),
            (CODEX_CA_CERT_ENV, "/tmp/codex.pem"),
            (SSL_CERT_FILE_ENV, "/tmp/fallback.pem"),
        ]);

        assert_eq!(
            env.configured_ca_bundle().map(|bundle| bundle.path),
            Some(PathBuf::from("/tmp/codex.pem"))
        );
    }

    #[test]
    fn ca_path_falls_back_to_ssl_cert_file() {
        let env = map_env(&[(SSL_CERT_FILE_ENV, "/tmp/fallback.pem")]);

        assert_eq!(
            env.configured_ca_bundle().map(|bundle| bundle.path),
            Some(PathBuf::from("/tmp/fallback.pem"))
        );
    }

    #[test]
    fn empty_ca_env_values_are_ignored() {
        let env = map_env(&[(SAVFOX_CA_CERT_ENV, ""), (CODEX_CA_CERT_ENV, "")]);

        assert!(env.configured_ca_bundle().is_none());
    }

    #[test]
    fn empty_ca_file_reports_invalid_bundle() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("empty.pem");
        std::fs::write(&path, "").expect("write pem");
        let bundle = ConfiguredCaBundle {
            source_env: SAVFOX_CA_CERT_ENV,
            path,
        };

        let error = bundle.load_certificates().expect_err("invalid bundle");
        assert!(matches!(
            error,
            BuildCustomCaTransportError::InvalidCaFile { .. }
        ));
    }

    #[test]
    fn trusted_certificate_der_trims_trailing_aux_data() {
        let der = [0x30, 0x03, 0x01, 0x02, 0x03, 0xff, 0xff];

        assert_eq!(first_der_item(&der), Some(&der[..5]));
    }
}

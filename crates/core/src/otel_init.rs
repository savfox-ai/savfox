use std::error::Error;

use savfox_otel::config::{
    OtelExporter, OtelHttpProtocol, OtelSettings, OtelTlsConfig as OtelTlsSettings,
};
use savfox_otel::otel_provider::OtelProvider;

use crate::config::Config;
use crate::config::types::{OtelExporterKind as Kind, OtelHttpProtocol as Protocol};
use crate::default_client::originator;
use crate::features::Feature;

/// Build an OpenTelemetry provider from the app Config.
///
/// Returns `None` when OTEL export is disabled.
pub fn build_provider(
    config: &Config,
    service_version: &str,
    service_name_override: Option<&str>,
    default_analytics_enabled: bool,
) -> Result<Option<OtelProvider>, Box<dyn Error>> {
    let to_otel_exporter = |kind: &Kind| match kind {
        Kind::None => OtelExporter::None,
        Kind::Statsig => OtelExporter::Statsig,
        Kind::OtlpHttp {
            endpoint,
            headers,
            protocol,
            tls,
        } => {
            let protocol = match protocol {
                Protocol::Json => OtelHttpProtocol::Json,
                Protocol::Binary => OtelHttpProtocol::Binary,
            };

            OtelExporter::OtlpHttp {
                endpoint: endpoint.clone(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                protocol,
                tls: tls.as_ref().map(|config| OtelTlsSettings {
                    ca_certificate: config.ca_certificate.clone(),
                    client_certificate: config.client_certificate.clone(),
                    client_private_key: config.client_private_key.clone(),
                }),
            }
        }
        Kind::OtlpGrpc {
            endpoint,
            headers,
            tls,
        } => OtelExporter::OtlpGrpc {
            endpoint: endpoint.clone(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            tls: tls.as_ref().map(|config| OtelTlsSettings {
                ca_certificate: config.ca_certificate.clone(),
                client_certificate: config.client_certificate.clone(),
                client_private_key: config.client_private_key.clone(),
            }),
        },
    };

    let exporter = to_otel_exporter(&config.otel.exporter);
    let trace_exporter = to_otel_exporter(&config.otel.trace_exporter);
    let metrics_exporter = if config
        .analytics_enabled
        .unwrap_or(default_analytics_enabled)
    {
        to_otel_exporter(&config.otel.metrics_exporter)
    } else {
        OtelExporter::None
    };

    let originator = originator();
    let service_name = service_name_override.unwrap_or(originator.value.as_str());
    let runtime_metrics = config.features.enabled(Feature::RuntimeMetrics);

    OtelProvider::from(&OtelSettings {
        service_name: service_name.to_string(),
        service_version: service_version.to_string(),
        savfox_home: config.savfox_home.clone(),
        environment: config.otel.environment.to_string(),
        exporter,
        trace_exporter,
        metrics_exporter,
        runtime_metrics,
    })
}

/// Filter predicate for exporting only Savfox-owned events via OTEL.
/// Keeps events that originated from savfox_otel module
pub fn savfox_export_filter(meta: &tracing::Metadata<'_>) -> bool {
    meta.target().starts_with("savfox_otel")
}

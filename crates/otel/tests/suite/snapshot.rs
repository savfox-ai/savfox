use std::collections::BTreeMap;

use opentelemetry_sdk::metrics::InMemoryMetricExporter;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::AuthMode;
use savfox_otel::OtelManager;
use savfox_otel::metrics::{MetricsClient, MetricsConfig, Result};
use savfox_protocol::SessionId;
use savfox_protocol::protocol::SessionSource;

use crate::harness::{attributes_to_map, find_metric};

#[test]
fn snapshot_collects_metrics_without_shutdown() -> Result<()> {
    let exporter = InMemoryMetricExporter::default();
    let config = MetricsConfig::in_memory(
        "test",
        "savfox-cli",
        env!("CARGO_PKG_VERSION"),
        exporter.clone(),
    )
    .with_tag("service", "savfox-cli")?
    .with_runtime_reader();
    let metrics = MetricsClient::new(config)?;

    metrics.counter(
        "savfox.tool.call",
        1,
        &[("tool", "shell"), ("success", "true")],
    )?;

    let snapshot = metrics.snapshot()?;

    let metric = find_metric(&snapshot, "savfox.tool.call").expect("counter metric missing");
    let attrs = match metric.data() {
        AggregatedMetrics::U64(data) => match data {
            MetricData::Sum(sum) => {
                let points: Vec<_> = sum.data_points().collect();
                assert_eq!(points.len(), 1);
                attributes_to_map(points[0].attributes())
            }
            _ => panic!("unexpected counter aggregation"),
        },
        _ => panic!("unexpected counter data type"),
    };

    let expected = BTreeMap::from([
        ("service".to_owned(), "savfox-cli".to_owned()),
        ("success".to_owned(), "true".to_owned()),
        ("tool".to_owned(), "shell".to_owned()),
    ]);
    assert_eq!(attrs, expected);

    let finished = exporter
        .get_finished_metrics()
        .expect("finished metrics should be readable");
    assert!(finished.is_empty(), "expected no periodic exports yet");

    Ok(())
}

#[test]
fn manager_snapshot_metrics_collects_without_shutdown() -> Result<()> {
    let exporter = InMemoryMetricExporter::default();
    let config =
        MetricsConfig::in_memory("test", "savfox-cli", env!("CARGO_PKG_VERSION"), exporter)
            .with_tag("service", "savfox-cli")?
            .with_runtime_reader();
    let metrics = MetricsClient::new(config)?;
    let manager = OtelManager::new(
        SessionId::new(),
        "gpt-5.1",
        "gpt-5.1",
        Some("account-id".to_owned()),
        None,
        Some(AuthMode::ApiKey),
        true,
        "tty".to_owned(),
        SessionSource::Cli,
    )
    .with_metrics(metrics);

    manager.counter(
        "savfox.tool.call",
        1,
        &[("tool", "shell"), ("success", "true")],
    );

    let snapshot = manager.snapshot_metrics()?;
    let metric = find_metric(&snapshot, "savfox.tool.call").expect("counter metric missing");
    let attrs = match metric.data() {
        AggregatedMetrics::U64(data) => match data {
            MetricData::Sum(sum) => {
                let points: Vec<_> = sum.data_points().collect();
                assert_eq!(points.len(), 1);
                attributes_to_map(points[0].attributes())
            }
            _ => panic!("unexpected counter aggregation"),
        },
        _ => panic!("unexpected counter data type"),
    };

    let expected = BTreeMap::from([
        (
            "app.version".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
        ),
        ("auth_mode".to_owned(), AuthMode::ApiKey.to_string()),
        ("model".to_owned(), "gpt-5.1".to_owned()),
        ("service".to_owned(), "savfox-cli".to_owned()),
        ("session_source".to_owned(), "cli".to_owned()),
        ("success".to_owned(), "true".to_owned()),
        ("tool".to_owned(), "shell".to_owned()),
    ]);
    assert_eq!(attrs, expected);

    Ok(())
}

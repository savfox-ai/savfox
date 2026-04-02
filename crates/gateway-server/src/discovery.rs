//! mDNS/Bonjour service discovery for the Savfox Gateway.
//!
//! Advertises the gateway on the local network so that native clients
//! (iOS, Android, macOS) can automatically discover it without manual
//! configuration.
//!
//! Service type: `_savfox._tcp.local.`
//! TXT records: version, protocol, name

use std::collections::HashMap;
use std::net::IpAddr;

use tracing::{info, warn};

/// mDNS service type for Savfox gateway discovery.
pub(crate) const SERVICE_TYPE: &str = "_savfox._tcp.local.";

/// Service advertisement info.
#[derive(Debug, Clone)]
pub(crate) struct ServiceInfo {
    /// Instance name (e.g., "My Savfox Gateway").
    pub(crate) name: String,
    /// Port the gateway is listening on.
    pub(crate) port: u16,
    /// Protocol version.
    pub(crate) protocol_version: u32,
    /// Additional TXT record data.
    pub(crate) txt_records: HashMap<String, String>,
}

/// Discovery configuration.
#[derive(Debug, Clone, Default)]
pub(crate) struct DiscoveryConfig {
    /// Enable mDNS advertisement.
    pub(crate) enabled: bool,
    /// Custom instance name (default: hostname).
    pub(crate) instance_name: Option<String>,
}

/// Advertise the gateway on the local network using mDNS.
///
/// This is a no-op stub when the `mdns` feature is not enabled.
/// To enable, add `mdns-sd` or `libmdns` as a dependency and implement
/// the platform-specific registration.
pub(crate) struct ServiceAdvertiser {
    config: DiscoveryConfig,
    service_info: ServiceInfo,
}

impl ServiceAdvertiser {
    pub(crate) fn new(config: DiscoveryConfig, service_info: ServiceInfo) -> Self {
        Self {
            config,
            service_info,
        }
    }

    /// Start advertising. Returns a handle that stops advertising on drop.
    pub(crate) async fn start(&self) -> anyhow::Result<()> {
        if !self.config.enabled {
            info!("mDNS service discovery disabled");
            return Ok(());
        }

        let instance_name = self
            .config
            .instance_name
            .clone()
            .unwrap_or_else(|| "Savfox Gateway".to_owned());

        info!(
            name = %instance_name,
            port = self.service_info.port,
            service_type = SERVICE_TYPE,
            "mDNS service discovery: advertising gateway"
        );

        // TODO: Implement actual mDNS registration using mdns-sd crate.
        // Example with mdns-sd:
        //   let mdns = ServiceDaemon::new()?;
        //   let service = ServiceInfo::new(
        //       SERVICE_TYPE, &instance_name, &hostname,
        //       "", self.service_info.port, txt_records
        //   )?;
        //   mdns.register(service)?;

        warn!(
            "mDNS service discovery: stub implementation  - add `mdns-sd` dependency for real mDNS"
        );

        Ok(())
    }

    /// Stop advertising.
    pub(crate) async fn stop(&self) {
        info!("mDNS service discovery: stopped advertising");
    }
}

impl std::fmt::Debug for ServiceAdvertiser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAdvertiser")
            .field("enabled", &self.config.enabled)
            .field("service_info", &self.service_info)
            .finish()
    }
}

/// Discover Savfox gateways on the local network.
///
/// Returns a list of discovered service instances.
pub(crate) struct ServiceBrowser {
    _config: DiscoveryConfig,
}

/// A discovered gateway instance.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredGateway {
    /// Instance name.
    pub(crate) name: String,
    /// IP addresses.
    pub(crate) addresses: Vec<IpAddr>,
    /// Port.
    pub(crate) port: u16,
    /// Protocol version from TXT record.
    pub(crate) protocol_version: Option<u32>,
    /// Additional TXT records.
    pub(crate) txt_records: HashMap<String, String>,
}

impl ServiceBrowser {
    pub(crate) fn new(config: DiscoveryConfig) -> Self {
        Self { _config: config }
    }

    /// Browse for gateways on the local network.
    ///
    /// Returns discovered instances within the timeout period.
    pub(crate) async fn browse(&self, timeout: std::time::Duration) -> Vec<DiscoveredGateway> {
        info!(
            timeout_ms = timeout.as_millis(),
            "mDNS: browsing for Savfox gateways"
        );

        // TODO: Implement actual mDNS browsing using mdns-sd crate.
        // Example:
        //   let mdns = ServiceDaemon::new()?;
        //   let receiver = mdns.browse(SERVICE_TYPE)?;
        //   while let Ok(event) = receiver.recv_timeout(timeout) { ... }

        warn!("mDNS browsing: stub implementation  - add `mdns-sd` dependency for real discovery");

        Vec::new()
    }
}

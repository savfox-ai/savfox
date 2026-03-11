import Foundation

/// Discovers Savfox gateways on the local network via Bonjour/mDNS.
public class GatewayDiscovery: NSObject, NetServiceBrowserDelegate, NetServiceDelegate {

    /// Bonjour service type for Savfox gateways.
    public static let serviceType = "_savfox-gw._tcp."

    /// Discovered gateways.
    public private(set) var gateways: [GatewayClient.DiscoveredGateway] = []

    /// Delegate for discovery events.
    public weak var delegate: GatewayDiscoveryDelegate?

    /// Net service browser.
    private let browser = NetServiceBrowser()

    /// Services being resolved.
    private var services: [NetService] = []

    // MARK: - Initialization

    public override init() {
        super.init()
        browser.delegate = self
    }

    // MARK: - Discovery

    /// Start searching for gateways on the local network.
    public func startDiscovery() {
        gateways.removeAll()
        browser.searchForServices(ofType: GatewayDiscovery.serviceType, inDomain: "local.")
    }

    /// Stop searching.
    public func stopDiscovery() {
        browser.stop()
        services.removeAll()
    }

    // MARK: - NetServiceBrowserDelegate

    public func netServiceBrowser(_ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool) {
        services.append(service)
        service.delegate = self
        service.resolve(withTimeout: 5.0)
    }

    public func netServiceBrowser(_ browser: NetServiceBrowser, didRemove service: NetService, moreComing: Bool) {
        services.removeAll { $0 == service }
        gateways.removeAll { $0.name == service.name }
        delegate?.gatewayDiscovery(self, didUpdateGateways: gateways)
    }

    // MARK: - NetServiceDelegate

    public func netServiceDidResolveAddress(_ sender: NetService) {
        guard let host = sender.hostName else { return }

        let gateway = GatewayClient.DiscoveredGateway(
            name: sender.name,
            host: host,
            port: sender.port,
            version: nil
        )

        gateways.append(gateway)
        delegate?.gatewayDiscovery(self, didDiscover: gateway)
        delegate?.gatewayDiscovery(self, didUpdateGateways: gateways)
    }
}

/// Delegate for gateway discovery events.
public protocol GatewayDiscoveryDelegate: AnyObject {
    func gatewayDiscovery(_ discovery: GatewayDiscovery, didDiscover gateway: GatewayClient.DiscoveredGateway)
    func gatewayDiscovery(_ discovery: GatewayDiscovery, didUpdateGateways gateways: [GatewayClient.DiscoveredGateway])
}

import Foundation

/// Client for connecting to an Savfox Gateway via WebSocket.
///
/// Handles:
/// - Token-based authentication
/// - JSON-RPC message framing
/// - Reconnection with exponential backoff
/// - mDNS/Bonjour discovery
public class GatewayClient: NSObject, URLSessionWebSocketDelegate {

    /// Connection state
    public enum State {
        case disconnected
        case connecting
        case authenticating
        case connected
        case error(Error)
    }

    /// Gateway discovery result
    public struct DiscoveredGateway {
        public let name: String
        public let host: String
        public let port: Int
        public let version: String?
    }

    /// Delegate for receiving gateway events
    public weak var delegate: GatewayClientDelegate?

    /// Current connection state
    public private(set) var state: State = .disconnected

    /// Gateway URL
    public let gatewayURL: URL

    /// Authentication token
    private let token: String

    /// WebSocket task
    private var webSocketTask: URLSessionWebSocketTask?

    /// URL session
    private lazy var session: URLSession = {
        URLSession(configuration: .default, delegate: self, delegateQueue: nil)
    }()

    /// Message ID counter
    private var nextMessageId: Int = 1

    /// Reconnection attempt count
    private var reconnectAttempts: Int = 0

    /// Maximum reconnection attempts
    public var maxReconnectAttempts: Int = 10

    /// Base reconnection delay in seconds
    public var baseReconnectDelay: TimeInterval = 1.0

    // MARK: - Initialization

    /// Create a new gateway client.
    /// - Parameters:
    ///   - url: Gateway WebSocket URL (e.g., "ws://192.168.1.100:18881/ws")
    ///   - token: Authentication token
    public init(url: URL, token: String) {
        self.gatewayURL = url
        self.token = token
        super.init()
    }

    /// Create from a discovered gateway.
    public convenience init(gateway: DiscoveredGateway, token: String, useTLS: Bool = false) {
        let scheme = useTLS ? "wss" : "ws"
        let url = URL(string: "\(scheme)://\(gateway.host):\(gateway.port)/ws")!
        self.init(url: url, token: token)
    }

    // MARK: - Connection

    /// Connect to the gateway.
    public func connect() {
        guard case .disconnected = state else { return }

        state = .connecting
        delegate?.gatewayClient(self, didChangeState: state)

        webSocketTask = session.webSocketTask(with: gatewayURL)
        webSocketTask?.resume()

        // Send authentication message
        authenticate()

        // Start receiving messages
        receiveMessage()
    }

    /// Disconnect from the gateway.
    public func disconnect() {
        webSocketTask?.cancel(with: .normalClosure, reason: nil)
        webSocketTask = nil
        state = .disconnected
        delegate?.gatewayClient(self, didChangeState: state)
    }

    // MARK: - Authentication

    private func authenticate() {
        state = .authenticating

        let authMessage: [String: Any] = [
            "method": "auth",
            "params": ["token": token],
            "id": nextId()
        ]

        send(authMessage)
    }

    // MARK: - Messaging

    /// Send a chat message.
    /// - Parameters:
    ///   - text: Message text
    ///   - sessionId: Optional session ID
    public func sendChatMessage(_ text: String, sessionId: String? = nil) {
        var params: [String: Any] = ["text": text]
        if let sessionId = sessionId {
            params["session_id"] = sessionId
        }

        let message: [String: Any] = [
            "method": "chat.send",
            "params": params,
            "id": nextId()
        ]

        send(message)
    }

    /// Send a JSON-RPC request.
    /// - Parameters:
    ///   - method: RPC method name
    ///   - params: Parameters dictionary
    public func sendRPC(method: String, params: [String: Any] = [:]) {
        let message: [String: Any] = [
            "method": method,
            "params": params,
            "id": nextId()
        ]

        send(message)
    }

    // MARK: - Private

    private func send(_ message: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: message),
              let text = String(data: data, encoding: .utf8) else { return }

        webSocketTask?.send(.string(text)) { [weak self] error in
            if let error = error {
                self?.handleError(error)
            }
        }
    }

    private func receiveMessage() {
        webSocketTask?.receive { [weak self] result in
            guard let self = self else { return }

            switch result {
            case .success(let message):
                switch message {
                case .string(let text):
                    self.handleTextMessage(text)
                case .data(let data):
                    if let text = String(data: data, encoding: .utf8) {
                        self.handleTextMessage(text)
                    }
                @unknown default:
                    break
                }
                // Continue receiving
                self.receiveMessage()

            case .failure(let error):
                self.handleError(error)
            }
        }
    }

    private func handleTextMessage(_ text: String) {
        guard let data = text.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }

        // Check for auth success
        if let result = json["result"] as? [String: Any],
           result["authenticated"] as? Bool == true {
            state = .connected
            reconnectAttempts = 0
            delegate?.gatewayClient(self, didChangeState: state)
            return
        }

        // Forward to delegate
        delegate?.gatewayClient(self, didReceiveMessage: json)
    }

    private func handleError(_ error: Error) {
        state = .error(error)
        delegate?.gatewayClient(self, didChangeState: state)

        // Attempt reconnection
        scheduleReconnect()
    }

    private func scheduleReconnect() {
        guard reconnectAttempts < maxReconnectAttempts else {
            delegate?.gatewayClient(self, didFailPermanently: NSError(
                domain: "SavfoxKit",
                code: -1,
                userInfo: [NSLocalizedDescriptionKey: "Max reconnection attempts reached"]
            ))
            return
        }

        let delay = baseReconnectDelay * pow(2.0, Double(reconnectAttempts))
        reconnectAttempts += 1

        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            self?.state = .disconnected
            self?.connect()
        }
    }

    private func nextId() -> Int {
        let id = nextMessageId
        nextMessageId += 1
        return id
    }
}

/// Delegate protocol for GatewayClient events.
public protocol GatewayClientDelegate: AnyObject {
    func gatewayClient(_ client: GatewayClient, didChangeState state: GatewayClient.State)
    func gatewayClient(_ client: GatewayClient, didReceiveMessage message: [String: Any])
    func gatewayClient(_ client: GatewayClient, didFailPermanently error: Error)
}

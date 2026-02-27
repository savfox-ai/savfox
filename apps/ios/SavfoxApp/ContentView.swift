import SwiftUI

/// Main entry point for the Savfox iOS app.
@main
struct SavfoxApp: App {
    @StateObject private var appState = AppState()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(appState)
        }
    }
}

/// Root content view — shows either connection setup or chat.
struct ContentView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        NavigationStack {
            switch appState.connectionState {
            case .disconnected:
                ConnectionView()
            case .connected:
                ChatView()
            case .discovering:
                DiscoveryView()
            }
        }
    }
}

/// Connection setup view — enter gateway URL and token.
struct ConnectionView: View {
    @EnvironmentObject var appState: AppState
    @State private var gatewayURL = ""
    @State private var token = ""
    @State private var showScanner = false

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 64))
                .foregroundColor(.blue)

            Text("Connect to Gateway")
                .font(.title2)
                .fontWeight(.semibold)

            VStack(spacing: 16) {
                TextField("Gateway URL", text: $gatewayURL)
                    .textFieldStyle(.roundedBorder)
                    .autocapitalization(.none)
                    .keyboardType(.URL)

                SecureField("Token", text: $token)
                    .textFieldStyle(.roundedBorder)
            }
            .padding(.horizontal)

            Button("Connect") {
                appState.connect(url: gatewayURL, token: token)
            }
            .buttonStyle(.borderedProminent)
            .disabled(gatewayURL.isEmpty || token.isEmpty)

            Divider()

            Button {
                appState.connectionState = .discovering
            } label: {
                Label("Discover on Network", systemImage: "bonjour")
            }

            Button {
                showScanner = true
            } label: {
                Label("Scan QR Code", systemImage: "qrcode.viewfinder")
            }
        }
        .padding()
        .navigationTitle("Savfox")
    }
}

/// Bonjour discovery view.
struct DiscoveryView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        List {
            Section("Searching...") {
                ProgressView()
            }
        }
        .navigationTitle("Discover Gateway")
        .toolbar {
            Button("Cancel") {
                appState.connectionState = .disconnected
            }
        }
    }
}

/// Chat view — main conversation interface.
struct ChatView: View {
    @EnvironmentObject var appState: AppState
    @State private var messageText = ""
    @FocusState private var isInputFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            // Messages list
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(appState.messages) { message in
                            MessageBubble(message: message)
                                .id(message.id)
                        }
                    }
                    .padding()
                }
                .onChange(of: appState.messages.count) { _, _ in
                    if let last = appState.messages.last {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }

            Divider()

            // Input bar
            HStack(spacing: 12) {
                TextField("Message...", text: $messageText, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...5)
                    .focused($isInputFocused)

                Button {
                    guard !messageText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
                    appState.sendMessage(messageText)
                    messageText = ""
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .disabled(messageText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding()
        }
        .navigationTitle("Chat")
        .toolbar {
            Button {
                appState.disconnect()
            } label: {
                Image(systemName: "xmark.circle")
            }
        }
    }
}

/// A single message bubble.
struct MessageBubble: View {
    let message: ChatMessage

    var body: some View {
        HStack {
            if message.isUser { Spacer() }

            Text(message.text)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(message.isUser ? Color.blue : Color(.systemGray5))
                .foregroundColor(message.isUser ? .white : .primary)
                .clipShape(RoundedRectangle(cornerRadius: 16))

            if !message.isUser { Spacer() }
        }
    }
}

// MARK: - Models

/// App-level state.
class AppState: ObservableObject {
    enum ConnectionState {
        case disconnected
        case discovering
        case connected
    }

    @Published var connectionState: ConnectionState = .disconnected
    @Published var messages: [ChatMessage] = []

    func connect(url: String, token: String) {
        // TODO: Initialize GatewayClient from SavfoxKit
        connectionState = .connected
    }

    func disconnect() {
        connectionState = .disconnected
        messages.removeAll()
    }

    func sendMessage(_ text: String) {
        messages.append(ChatMessage(text: text, isUser: true))
        // TODO: Send via GatewayClient
    }
}

/// A chat message.
struct ChatMessage: Identifiable {
    let id = UUID()
    let text: String
    let isUser: Bool
    let timestamp = Date()
}

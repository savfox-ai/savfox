import SwiftUI

/// macOS menu bar application for quick access to Savfox gateway.
@main
struct SavfoxMenuApp: App {
    @StateObject private var appState = MenuBarState()

    var body: some Scene {
        MenuBarExtra {
            MenuBarView()
                .environmentObject(appState)
        } label: {
            Image(systemName: appState.isConnected ? "antenna.radiowaves.left.and.right" : "antenna.radiowaves.left.and.right.slash")
        }

        // Settings window
        Settings {
            SettingsView()
                .environmentObject(appState)
        }
    }
}

/// Menu bar dropdown content.
struct MenuBarView: View {
    @EnvironmentObject var appState: MenuBarState

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Status
            HStack {
                Circle()
                    .fill(appState.isConnected ? Color.green : Color.red)
                    .frame(width: 8, height: 8)
                Text(appState.isConnected ? "Connected" : "Disconnected")
                    .font(.headline)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)

            Divider()

            if appState.isConnected {
                // Quick chat
                Button("Quick Chat...") {
                    appState.showQuickChat = true
                }
                .keyboardShortcut("n", modifiers: [.command])
                .padding(.horizontal, 16)
                .padding(.vertical, 4)

                // Recent sessions
                Menu("Recent Sessions") {
                    ForEach(appState.recentSessions, id: \.self) { session in
                        Button(session) {
                            appState.openSession(session)
                        }
                    }
                    if appState.recentSessions.isEmpty {
                        Text("No recent sessions")
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 4)

                // Models
                Menu("Active Model") {
                    ForEach(appState.availableModels, id: \.self) { model in
                        Button(model) {
                            appState.setModel(model)
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 4)

                Divider()

                // Gateway info
                Text("Gateway: \(appState.gatewayURL)")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 4)
            }

            Divider()

            // Actions
            if appState.isConnected {
                Button("Open Web UI") {
                    appState.openWebUI()
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 4)

                Button("Disconnect") {
                    appState.disconnect()
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 4)
            } else {
                Button("Connect...") {
                    appState.showConnectionSheet = true
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 4)
            }

            Divider()

            SettingsLink {
                Text("Settings...")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)

            Button("Quit Savfox") {
                NSApplication.shared.terminate(nil)
            }
            .keyboardShortcut("q")
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
        }
        .frame(width: 280)
    }
}

/// Settings window.
struct SettingsView: View {
    @EnvironmentObject var appState: MenuBarState

    var body: some View {
        TabView {
            GeneralSettingsView()
                .tabItem { Label("General", systemImage: "gear") }

            ConnectionSettingsView()
                .tabItem { Label("Connection", systemImage: "network") }
        }
        .frame(width: 450, height: 300)
    }
}

struct GeneralSettingsView: View {
    @AppStorage("launchAtLogin") var launchAtLogin = false
    @AppStorage("showInDock") var showInDock = false

    var body: some View {
        Form {
            Toggle("Launch at login", isOn: $launchAtLogin)
            Toggle("Show in Dock", isOn: $showInDock)
        }
        .padding()
    }
}

struct ConnectionSettingsView: View {
    @AppStorage("gatewayURL") var gatewayURL = "ws://localhost:18881/ws"
    @AppStorage("gatewayToken") var token = ""

    var body: some View {
        Form {
            TextField("Gateway URL", text: $gatewayURL)
            SecureField("Token", text: $token)
        }
        .padding()
    }
}

// MARK: - State

class MenuBarState: ObservableObject {
    @Published var isConnected = false
    @Published var gatewayURL = "localhost:18881"
    @Published var showQuickChat = false
    @Published var showConnectionSheet = false
    @Published var recentSessions: [String] = []
    @Published var availableModels: [String] = []

    func disconnect() {
        isConnected = false
    }

    func openSession(_ id: String) {
        // TODO: Open session in web UI or chat window
    }

    func setModel(_ model: String) {
        // TODO: Set active model via RPC
    }

    func openWebUI() {
        if let url = URL(string: "http://\(gatewayURL)") {
            NSWorkspace.shared.open(url)
        }
    }
}

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
            Image(systemName: "antenna.radiowaves.left.and.right.slash")
        }

        // Settings window
        Settings {
            SettingsView()
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
                    .fill(Color.orange)
                    .frame(width: 8, height: 8)
                Text("Native client unavailable")
                    .font(.headline)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)

            Divider()

            Text("Session opening, model switching, and quick chat are not wired to Gateway RPC in this build.")
                .font(.caption)
                .foregroundColor(.secondary)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)

            Divider()

            Button("Open Web UI") {
                appState.openWebUI()
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)

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
    func openWebUI() {
        let configured = UserDefaults.standard.string(forKey: "gatewayURL") ?? "ws://localhost:18881/ws"
        guard var components = URLComponents(string: configured) else { return }
        components.scheme = components.scheme == "wss" ? "https" : "http"
        components.path = ""
        components.query = nil
        components.fragment = nil
        if let url = components.url {
            NSWorkspace.shared.open(url)
        }
    }
}

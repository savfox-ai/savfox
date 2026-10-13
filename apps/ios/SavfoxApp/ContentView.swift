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

/// Root content view for the unavailable native transport notice.
struct ContentView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        NavigationStack {
            ConnectionView()
        }
    }
}

/// Connection setup view — enter gateway URL and token.
struct ConnectionView: View {
    @EnvironmentObject var appState: AppState
    @State private var gatewayURL = ""
    @State private var token = ""

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

            if let notice = appState.availabilityNotice {
                Text(notice)
                    .font(.footnote)
                    .foregroundColor(.orange)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            }

            Button {
                appState.reportUnavailable()
            } label: {
                Label("Discover on Network", systemImage: "bonjour")
            }

            Button {
                appState.reportUnavailable()
            } label: {
                Label("Scan QR Code", systemImage: "qrcode.viewfinder")
            }
        }
        .padding()
        .navigationTitle("Savfox")
    }
}

/// App-level state.
class AppState: ObservableObject {
    @Published var availabilityNotice: String?

    func connect(url: String, token: String) {
        _ = (url, token)
        reportUnavailable()
    }

    func reportUnavailable() {
        availabilityNotice = "Native Gateway connection and messaging are not available in this build. Use the Gateway web UI instead."
    }
}

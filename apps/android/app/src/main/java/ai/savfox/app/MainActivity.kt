package ai.savfox.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel

/**
 * Main entry point for the Savfox Android app.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            SavfoxTheme {
                SavfoxApp()
            }
        }
    }
}

@Composable
fun SavfoxApp(viewModel: AppViewModel = androidx.lifecycle.viewmodel.compose.viewModel()) {
    val uiState by viewModel.uiState.collectAsState()

    ConnectionScreen(
        notice = uiState.availabilityNotice,
        onConnect = { url, token -> viewModel.connect(url, token) },
        onDiscover = { viewModel.startDiscovery() }
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConnectionScreen(
    notice: String?,
    onConnect: (String, String) -> Unit,
    onDiscover: () -> Unit
) {
    var gatewayUrl by remember { mutableStateOf("") }
    var token by remember { mutableStateOf("") }

    Scaffold(
        topBar = { TopAppBar(title = { Text("Savfox") }) }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            Text("Connect to Gateway", style = MaterialTheme.typography.headlineMedium)

            Spacer(modifier = Modifier.height(32.dp))

            OutlinedTextField(
                value = gatewayUrl,
                onValueChange = { gatewayUrl = it },
                label = { Text("Gateway URL") },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true
            )

            Spacer(modifier = Modifier.height(16.dp))

            OutlinedTextField(
                value = token,
                onValueChange = { token = it },
                label = { Text("Token") },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true
            )

            Spacer(modifier = Modifier.height(24.dp))

            if (notice != null) {
                Text(
                    text = notice,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall
                )
                Spacer(modifier = Modifier.height(16.dp))
            }

            Button(
                onClick = { onConnect(gatewayUrl, token) },
                enabled = gatewayUrl.isNotBlank() && token.isNotBlank(),
                modifier = Modifier.fillMaxWidth()
            ) {
                Text("Connect")
            }

            Spacer(modifier = Modifier.height(16.dp))

            OutlinedButton(onClick = onDiscover, modifier = Modifier.fillMaxWidth()) {
                Text("Discover on Network")
            }
        }
    }
}

data class AppUiState(
    val availabilityNotice: String? = null
)

class AppViewModel : ViewModel() {
    private val _uiState = kotlinx.coroutines.flow.MutableStateFlow(AppUiState())
    val uiState = _uiState.asStateFlow()

    fun connect(url: String, token: String) {
        if (url.isNotBlank() && token.isNotBlank()) {
            reportUnavailable()
        }
    }

    fun startDiscovery() {
        reportUnavailable()
    }

    private fun reportUnavailable() {
        _uiState.value = _uiState.value.copy(
            availabilityNotice = "Native Gateway connection and messaging are not available in this build. Use the Gateway web UI instead."
        )
    }
}

@Composable
fun SavfoxTheme(content: @Composable () -> Unit) {
    MaterialTheme(content = content)
}

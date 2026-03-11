package ai.savfox.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.launch
import java.util.UUID

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
fun SavfoxApp(viewModel: ChatViewModel = androidx.lifecycle.viewmodel.compose.viewModel()) {
    val uiState by viewModel.uiState.collectAsState()

    when (uiState.connectionState) {
        ConnectionState.DISCONNECTED -> ConnectionScreen(
            onConnect = { url, token -> viewModel.connect(url, token) },
            onDiscover = { viewModel.startDiscovery() }
        )
        ConnectionState.DISCOVERING -> DiscoveryScreen(
            onCancel = { viewModel.cancelDiscovery() }
        )
        ConnectionState.CONNECTED -> ChatScreen(
            messages = uiState.messages,
            onSendMessage = { viewModel.sendMessage(it) },
            onDisconnect = { viewModel.disconnect() }
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConnectionScreen(onConnect: (String, String) -> Unit, onDiscover: () -> Unit) {
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

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DiscoveryScreen(onCancel: () -> Unit) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Discover Gateway") },
                actions = { TextButton(onClick = onCancel) { Text("Cancel") } }
            )
        }
    ) { padding ->
        Box(
            modifier = Modifier.fillMaxSize().padding(padding),
            contentAlignment = Alignment.Center
        ) {
            CircularProgressIndicator()
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(
    messages: List<ChatMessage>,
    onSendMessage: (String) -> Unit,
    onDisconnect: () -> Unit
) {
    var messageText by remember { mutableStateOf("") }
    val listState = rememberLazyListState()
    val coroutineScope = rememberCoroutineScope()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Chat") },
                actions = {
                    IconButton(onClick = onDisconnect) {
                        Text("X")
                    }
                }
            )
        },
        bottomBar = {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(8.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                OutlinedTextField(
                    value = messageText,
                    onValueChange = { messageText = it },
                    modifier = Modifier.weight(1f),
                    placeholder = { Text("Message...") },
                    maxLines = 4
                )
                Spacer(modifier = Modifier.width(8.dp))
                Button(
                    onClick = {
                        if (messageText.isNotBlank()) {
                            onSendMessage(messageText)
                            messageText = ""
                        }
                    },
                    enabled = messageText.isNotBlank()
                ) {
                    Text("Send")
                }
            }
        }
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 16.dp),
            state = listState,
            verticalArrangement = Arrangement.spacedBy(8.dp)
        ) {
            items(messages) { message ->
                MessageBubble(message = message)
            }
        }

        LaunchedEffect(messages.size) {
            if (messages.isNotEmpty()) {
                coroutineScope.launch {
                    listState.animateScrollToItem(messages.size - 1)
                }
            }
        }
    }
}

@Composable
fun MessageBubble(message: ChatMessage) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (message.isUser) Arrangement.End else Arrangement.Start
    ) {
        Surface(
            shape = MaterialTheme.shapes.medium,
            color = if (message.isUser) MaterialTheme.colorScheme.primary
                    else MaterialTheme.colorScheme.surfaceVariant,
            tonalElevation = 1.dp
        ) {
            Text(
                text = message.text,
                modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
                color = if (message.isUser) MaterialTheme.colorScheme.onPrimary
                        else MaterialTheme.colorScheme.onSurfaceVariant
            )
        }
    }
}

// Models

enum class ConnectionState { DISCONNECTED, DISCOVERING, CONNECTED }

data class ChatMessage(
    val id: String = UUID.randomUUID().toString(),
    val text: String,
    val isUser: Boolean,
    val timestamp: Long = System.currentTimeMillis()
)

data class ChatUiState(
    val connectionState: ConnectionState = ConnectionState.DISCONNECTED,
    val messages: List<ChatMessage> = emptyList()
)

class ChatViewModel : ViewModel() {
    private val _uiState = kotlinx.coroutines.flow.MutableStateFlow(ChatUiState())
    val uiState = _uiState.asStateFlow()

    fun connect(url: String, token: String) {
        // TODO: Initialize GatewayClient
        _uiState.value = _uiState.value.copy(connectionState = ConnectionState.CONNECTED)
    }

    fun disconnect() {
        _uiState.value = ChatUiState()
    }

    fun startDiscovery() {
        _uiState.value = _uiState.value.copy(connectionState = ConnectionState.DISCOVERING)
    }

    fun cancelDiscovery() {
        _uiState.value = _uiState.value.copy(connectionState = ConnectionState.DISCONNECTED)
    }

    fun sendMessage(text: String) {
        val message = ChatMessage(text = text, isUser = true)
        _uiState.value = _uiState.value.copy(
            messages = _uiState.value.messages + message
        )
        // TODO: Send via GatewayClient
    }
}

@Composable
fun SavfoxTheme(content: @Composable () -> Unit) {
    MaterialTheme(content = content)
}

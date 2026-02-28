use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, broadcast};
use tracing::{debug, info};

use super::a2ui::{A2UIAction, A2UIComponent, A2UIState};
use super::websocket::CanvasMessage;

pub struct CanvasHostService {
    states: Arc<RwLock<HashMap<String, A2UIState>>>,
    action_tx: broadcast::Sender<A2UIAction>,
    update_tx: broadcast::Sender<CanvasMessage>,
}

impl CanvasHostService {
    pub fn new() -> Self {
        let (action_tx, _) = broadcast::channel(64);
        let (update_tx, _) = broadcast::channel(64);

        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            action_tx,
            update_tx,
        }
    }

    pub async fn push(&self, surface_id: &str, component: A2UIComponent) {
        let component_json = serde_json::to_value(&component).ok();

        {
            let mut states = self.states.write().await;
            let state = states
                .entry(surface_id.to_string())
                .or_insert_with(|| A2UIState::new(surface_id));
            state.set_root(component);
        }

        if let Some(json) = component_json {
            let msg = CanvasMessage::push(surface_id, json);
            let _ = self.update_tx.send(msg);
        }

        info!("Canvas push: surface={}", surface_id);
    }

    pub async fn reset(&self, surface_id: &str) {
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(surface_id) {
                state.clear();
            }
        }

        let msg = CanvasMessage::reset(surface_id);
        let _ = self.update_tx.send(msg);

        info!("Canvas reset: surface={}", surface_id);
    }

    pub async fn get_state(&self, surface_id: &str) -> Option<A2UIState> {
        let states = self.states.read().await;
        states.get(surface_id).cloned()
    }

    pub fn subscribe_updates(&self) -> broadcast::Receiver<CanvasMessage> {
        self.update_tx.subscribe()
    }

    pub fn subscribe_actions(&self) -> broadcast::Receiver<A2UIAction> {
        self.action_tx.subscribe()
    }

    pub async fn handle_action(&self, action: A2UIAction) {
        info!(
            "Canvas action: name={}, surface={}",
            action.name, action.surface_id
        );
        let _ = self.action_tx.send(action);
    }

    pub fn render_index_html() -> String {
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Savfox Canvas</title>
    <style>
        :root {
            --bg-primary: #0a0a0a;
            --bg-secondary: #141414;
            --bg-tertiary: #1e1e1e;
            --border: #2e2e2e;
            --text-primary: #e5e5e5;
            --text-secondary: #999;
            --accent: #3b82f6;
            --radius: 8px;
        }
        * { margin: 0; padding: 0; box-sizing: border-box; }
        html, body { height: 100%; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: var(--bg-primary); color: var(--text-primary); }
        .container { max-width: 800px; margin: 0 auto; padding: 24px; }
        h1 { font-size: 24px; margin-bottom: 8px; }
        .status { color: var(--text-secondary); font-size: 14px; margin-bottom: 24px; }
        .status.ok { color: #22c55e; }
        .status.error { color: #ef4444; }
        .surface { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: var(--radius); padding: 20px; min-height: 200px; }
        .component { margin: 8px 0; }
        .component-heading { font-weight: 600; margin: 16px 0 8px; }
        .component-heading[data-level="1"] { font-size: 24px; }
        .component-heading[data-level="2"] { font-size: 20px; }
        .component-heading[data-level="3"] { font-size: 18px; }
        .component-text { line-height: 1.6; }
        .component-button { background: var(--accent); color: #fff; border: none; padding: 10px 20px; border-radius: var(--radius); cursor: pointer; margin: 4px; }
        .component-button:hover { opacity: 0.9; }
        .component-input { width: 100%; padding: 10px; background: var(--bg-tertiary); border: 1px solid var(--border); border-radius: var(--radius); color: var(--text-primary); }
        .component-image { max-width: 100%; border-radius: var(--radius); }
        .component-card { background: var(--bg-tertiary); border-radius: var(--radius); padding: 16px; margin: 8px 0; }
    </style>
</head>
<body>
    <div class="container">
        <h1>Savfox Canvas</h1>
        <div id="status" class="status">Connecting...</div>
        <div id="surface" class="surface"></div>
    </div>
    <script>
        let ws = null;
        let reconnectTimeout = null;
        
        function connect() {
            const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
            ws = new WebSocket(protocol + '//' + location.host + '/canvas/ws');
            
            ws.onopen = () => {
                document.getElementById('status').textContent = 'Connected';
                document.getElementById('status').className = 'status ok';
            };
            
            ws.onclose = () => {
                document.getElementById('status').textContent = 'Disconnected. Reconnecting...';
                document.getElementById('status').className = 'status error';
                if (reconnectTimeout) clearTimeout(reconnectTimeout);
                reconnectTimeout = setTimeout(connect, 3000);
            };
            
            ws.onerror = (err) => {
                document.getElementById('status').textContent = 'Connection error';
                document.getElementById('status').className = 'status error';
            };
            
            ws.onmessage = (event) => {
                try {
                    const msg = JSON.parse(event.data);
                    handleMessage(msg);
                } catch (e) {
                    console.error('Failed to parse message:', e);
                }
            };
        }
        
        function handleMessage(msg) {
            switch (msg.type) {
                case 'push':
                    renderComponent(msg.component);
                    break;
                case 'reset':
                    document.getElementById('surface').innerHTML = '';
                    break;
                case 'state_update':
                    if (msg.component) {
                        renderComponent(msg.component);
                    }
                    break;
            }
        }
        
        function renderComponent(component) {
            const el = createComponentElement(component);
            document.getElementById('surface').innerHTML = '';
            if (el) {
                document.getElementById('surface').appendChild(el);
            }
        }
        
        function createComponentElement(comp) {
            if (!comp) return null;
            
            let el;
            switch (comp.type) {
                case 'heading':
                    el = document.createElement('h' + (comp.props.level || 2));
                    el.className = 'component component-heading';
                    el.setAttribute('data-level', comp.props.level || 2);
                    el.textContent = comp.props.content || '';
                    break;
                case 'text':
                    el = document.createElement('p');
                    el.className = 'component component-text';
                    el.textContent = comp.props.content || '';
                    break;
                case 'button':
                    el = document.createElement('button');
                    el.className = 'component component-button';
                    el.textContent = comp.props.label || '';
                    el.onclick = () => sendAction(comp.props.action, comp.id);
                    break;
                case 'input':
                    el = document.createElement('input');
                    el.className = 'component component-input';
                    el.placeholder = comp.props.placeholder || '';
                    break;
                case 'image':
                    el = document.createElement('img');
                    el.className = 'component component-image';
                    el.src = comp.props.src || '';
                    el.alt = comp.props.alt || '';
                    break;
                case 'card':
                    el = document.createElement('div');
                    el.className = 'component component-card';
                    if (comp.children) {
                        comp.children.forEach(child => {
                            const childEl = createComponentElement(child);
                            if (childEl) el.appendChild(childEl);
                        });
                    }
                    break;
                case 'list':
                    el = document.createElement('div');
                    el.className = 'component';
                    if (comp.children) {
                        comp.children.forEach(child => {
                            const childEl = createComponentElement(child);
                            if (childEl) el.appendChild(childEl);
                        });
                    }
                    break;
                default:
                    el = document.createElement('div');
                    el.className = 'component';
                    el.textContent = JSON.stringify(comp);
            }
            
            return el;
        }
        
        function sendAction(name, componentId) {
            if (!ws || ws.readyState !== WebSocket.OPEN) return;
            
            const msg = {
                type: 'action',
                name: name,
                surface_id: 'main',
                source_component_id: componentId,
                context: { t: Date.now() }
            };
            ws.send(JSON.stringify(msg));
        }
        
        connect();
    </script>
</body>
</html>"#.to_string()
    }
}

impl Default for CanvasHostService {
    fn default() -> Self {
        Self::new()
    }
}

// ── Browser ─────────────────────────────────────────────────────────────────

const DEFAULT_BROWSER_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrowserProfileSettings {
    #[serde(default)]
    user_data_dir: Option<String>,
    #[serde(default)]
    executable_path: Option<String>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default = "default_browser_profile_headless")]
    headless: bool,
}

impl Default for BrowserProfileSettings {
    fn default() -> Self {
        Self {
            user_data_dir: None,
            executable_path: None,
            extensions: Vec::new(),
            args: Vec::new(),
            proxy: None,
            headless: default_browser_profile_headless(),
        }
    }
}

fn default_browser_profile_name() -> String {
    "default".to_string()
}

fn default_browser_profile_headless() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrowserProfilesConfig {
    #[serde(default = "default_browser_profile_name")]
    default_profile: String,
    #[serde(default)]
    profiles: HashMap<String, BrowserProfileSettings>,
}

impl Default for BrowserProfilesConfig {
    fn default() -> Self {
        Self {
            default_profile: default_browser_profile_name(),
            profiles: HashMap::new(),
        }
    }
}

struct BrowserRuntimeSession {
    browser: Arc<Browser>,
    active_target_id: Option<String>,
}

#[derive(Default)]
struct BrowserRuntimeStore {
    sessions: HashMap<String, BrowserRuntimeSession>,
}

fn browser_runtime_store() -> &'static Mutex<BrowserRuntimeStore> {
    static STORE: OnceLock<Mutex<BrowserRuntimeStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(BrowserRuntimeStore::default()))
}

fn browser_profiles_path(channel: &GatewayChannel) -> PathBuf {
    channel.config().savfox_home.join("browser-profiles.json")
}

fn browser_profile_default_dir(channel: &GatewayChannel, profile: &str) -> PathBuf {
    channel
        .config()
        .savfox_home
        .join("browser")
        .join("profiles")
        .join(profile)
}

async fn load_browser_profiles_config(channel: &GatewayChannel) -> BrowserProfilesConfig {
    let path = browser_profiles_path(channel);
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => BrowserProfilesConfig::default(),
    }
}

async fn save_browser_profiles_config(
    channel: &GatewayChannel,
    cfg: &BrowserProfilesConfig,
) -> Result<(), (i64, String)> {
    let path = browser_profiles_path(channel);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to create profile dir: {e}")))?;
    }
    let encoded = serde_json::to_string_pretty(cfg).map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to encode browser profiles: {e}"),
        )
    })?;
    tokio::fs::write(path, encoded).await.map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to persist browser profiles: {e}"),
        )
    })?;
    Ok(())
}

fn validate_browser_profile_name(name: &str) -> Result<String, (i64, String)> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err((INVALID_PARAMS, "profile name cannot be empty".to_string()));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err((
            INVALID_PARAMS,
            "profile name contains invalid characters".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn browser_timeout_ms(params: &Value) -> u64 {
    params
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_BROWSER_TIMEOUT_MS)
        .max(1)
}

fn selected_profile_name(
    params: &Value,
    cfg: &BrowserProfilesConfig,
) -> Result<String, (i64, String)> {
    let raw = params
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or(cfg.default_profile.as_str());
    validate_browser_profile_name(raw)
}

fn browser_profile_settings_from_params(
    params: &Value,
    current: BrowserProfileSettings,
) -> BrowserProfileSettings {
    let mut next = current;
    if let Some(path) = params.get("user_data_dir").and_then(|v| v.as_str()) {
        let trimmed = path.trim();
        next.user_data_dir = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(path) = params.get("executable_path").and_then(|v| v.as_str()) {
        let trimmed = path.trim();
        next.executable_path = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(exts) = params.get("extensions").and_then(|v| v.as_array()) {
        next.extensions = exts
            .iter()
            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
            .collect();
    }
    if let Some(args) = params.get("args").and_then(|v| v.as_array()) {
        next.args = args
            .iter()
            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
            .collect();
    }
    if let Some(proxy) = params.get("proxy").and_then(|v| v.as_str()) {
        let trimmed = proxy.trim();
        next.proxy = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(headless) = params.get("headless").and_then(|v| v.as_bool()) {
        next.headless = headless;
    }
    next
}

fn browser_launch_options_for_profile(
    channel: &GatewayChannel,
    profile: &str,
    settings: &BrowserProfileSettings,
) -> BrowserLaunchOptions {
    let user_data_dir = settings
        .user_data_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| browser_profile_default_dir(channel, profile));
    let executable_path = settings.executable_path.as_ref().map(PathBuf::from);

    let mut extra_args = Vec::new();
    if let Some(proxy) = settings.proxy.as_ref() {
        extra_args.push(format!("--proxy-server={proxy}"));
    }
    if !settings.extensions.is_empty() {
        extra_args.push(format!(
            "--load-extension={}",
            settings.extensions.join(",")
        ));
    }
    extra_args.extend(settings.args.clone());

    BrowserLaunchOptions {
        executable_path,
        user_data_dir: Some(user_data_dir),
        headless: settings.headless,
        extra_args,
    }
}

async fn browser_session_browser(profile: &str) -> Result<Arc<Browser>, (i64, String)> {
    let store = browser_runtime_store().lock().await;
    store
        .sessions
        .get(profile)
        .map(|s| s.browser.clone())
        .ok_or_else(|| {
            (
                INVALID_PARAMS,
                format!("browser profile '{profile}' is not started"),
            )
        })
}

async fn ensure_browser_session_for_profile(
    channel: &GatewayChannel,
    profile: &str,
    settings: &BrowserProfileSettings,
) -> Result<(), (i64, String)> {
    {
        let store = browser_runtime_store().lock().await;
        if store.sessions.contains_key(profile) {
            return Ok(());
        }
    }

    let options = browser_launch_options_for_profile(channel, profile, settings);
    if let Some(dir) = options.user_data_dir.as_ref() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| {
            (
                INTERNAL_ERROR,
                format!("failed to create profile data dir: {e}"),
            )
        })?;
    }

    let browser = Arc::new(
        Browser::launch_with_launch_options(options)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to launch browser: {e}")))?,
    );

    let mut store = browser_runtime_store().lock().await;
    match store.sessions.entry(profile.to_string()) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(BrowserRuntimeSession {
                browser,
                active_target_id: None,
            });
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(_) => Ok(()),
    }
}

async fn with_browser_timeout<T>(
    timeout_ms: u64,
    action: &'static str,
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> Result<T, (i64, String)> {
    let timeout = Duration::from_millis(timeout_ms.max(1));
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err((INTERNAL_ERROR, format!("{action} failed: {err}"))),
        Err(_) => Err((
            INTERNAL_ERROR,
            format!("{action} timed out after {} ms", timeout.as_millis()),
        )),
    }
}

fn requested_target_id(params: &Value) -> Option<String> {
    params
        .get("target_id")
        .and_then(|v| v.as_str())
        .or_else(|| params.get("tab_id").and_then(|v| v.as_str()))
        .map(ToOwned::to_owned)
}

async fn set_active_browser_target(profile: &str, target_id: Option<String>) {
    let mut store = browser_runtime_store().lock().await;
    if let Some(session) = store.sessions.get_mut(profile) {
        session.active_target_id = target_id;
    }
}

async fn active_browser_target(profile: &str) -> Option<String> {
    let store = browser_runtime_store().lock().await;
    store
        .sessions
        .get(profile)
        .and_then(|session| session.active_target_id.clone())
}

async fn select_browser_page(
    profile: &str,
    timeout_ms: u64,
    requested_target: Option<String>,
) -> Result<savfox_browser_automation::Page, (i64, String)> {
    let (browser, current_target_id) = {
        let store = browser_runtime_store().lock().await;
        let session = store.sessions.get(profile).ok_or_else(|| {
            (
                INVALID_PARAMS,
                format!("browser profile '{profile}' is not started"),
            )
        })?;
        (session.browser.clone(), session.active_target_id.clone())
    };

    let pages = with_browser_timeout(timeout_ms, "list browser tabs", browser.pages()).await?;
    if pages.is_empty() {
        let page = with_browser_timeout(timeout_ms, "open browser tab", browser.new_page()).await?;
        set_active_browser_target(profile, Some(page.target_id().to_string())).await;
        return Ok(page);
    }

    let preferred = requested_target.clone().or(current_target_id);
    let mut iter = pages.into_iter();
    let mut selected = iter
        .next()
        .ok_or_else(|| (INTERNAL_ERROR, "failed to select browser tab".to_string()))?;
    if let Some(target_id) = preferred {
        if selected.target_id() != target_id {
            let mut found = false;
            for page in iter {
                if page.target_id() == target_id {
                    selected = page;
                    found = true;
                    break;
                }
            }
            if !found && requested_target.is_some() {
                return Err((INVALID_PARAMS, format!("tab '{target_id}' not found")));
            }
        }
    }

    set_active_browser_target(profile, Some(selected.target_id().to_string())).await;
    Ok(selected)
}

fn is_xpath_selector(selector: &str) -> Option<&str> {
    selector.strip_prefix("xpath=").map(str::trim)
}

async fn resolve_browser_profile(
    params: &Value,
    channel: &GatewayChannel,
) -> Result<(String, BrowserProfileSettings), (i64, String)> {
    let cfg = load_browser_profiles_config(channel).await;
    let profile = selected_profile_name(params, &cfg)?;
    let settings = cfg.profiles.get(&profile).cloned().unwrap_or_default();
    Ok((profile, settings))
}

async fn handle_browser_request(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        return Err((INVALID_PARAMS, "missing 'url' parameter".to_string()));
    }

    let method_raw = params
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let _method = reqwest::Method::from_bytes(method_raw.as_bytes()).map_err(|e| {
        (
            INVALID_PARAMS,
            format!("invalid method '{method_raw}': {e}"),
        )
    })?;

    let use_browser_context = params
        .get("use_browser_context")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let fallback_direct = params
        .get("fallback_direct")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if use_browser_context {
        let timeout_ms = browser_timeout_ms(params);
        let (profile, settings) = resolve_browser_profile(params, channel).await?;
        ensure_browser_session_for_profile(channel, &profile, &settings).await?;
        let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

        let headers = params.get("headers").cloned().unwrap_or_else(|| json!({}));
        let body = params.get("body").cloned().unwrap_or(Value::Null);
        let url_json = serde_json::to_string(url)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode url: {e}")))?;
        let method_json = serde_json::to_string(&method_raw)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode method: {e}")))?;
        let headers_json = serde_json::to_string(&headers)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode headers: {e}")))?;
        let body_json = serde_json::to_string(&body)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to encode body: {e}")))?;

        let expr = format!(
            r#"(async () => {{
                const targetUrl = {url_json};
                const method = {method_json};
                const headers = {headers_json};
                const body = {body_json};
                const init = {{ method, headers, credentials: "include" }};
                if (body !== null && body !== undefined) {{
                    init.body = typeof body === "string" ? body : JSON.stringify(body);
                    if (!headers["Content-Type"] && !headers["content-type"] && typeof body !== "string") {{
                        init.headers = {{ ...headers, "Content-Type": "application/json" }};
                    }}
                }}
                try {{
                    const resp = await fetch(targetUrl, init);
                    const text = await resp.text();
                    const headerMap = Object.fromEntries(resp.headers.entries());
                    return {{
                        ok: true,
                        status: resp.status,
                        url: resp.url,
                        content_type: headerMap["content-type"] || null,
                        headers: headerMap,
                        body: text
                    }};
                }} catch (err) {{
                    return {{ ok: false, error: String(err) }};
                }}
            }})()"#
        );

        let browser_result =
            with_browser_timeout(timeout_ms, "browser request", page.evaluate(&expr)).await?;
        if browser_result
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let body = browser_result
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let truncated = body.chars().count() > 100_000;
            let body = if truncated {
                body.chars().take(100_000).collect::<String>()
            } else {
                body
            };
            return Ok(json!({
                "status": browser_result.get("status").cloned().unwrap_or(json!(0)),
                "url": browser_result.get("url").cloned().unwrap_or(json!(url)),
                "content_type": browser_result.get("content_type").cloned().unwrap_or(Value::Null),
                "headers": browser_result.get("headers").cloned().unwrap_or_else(|| json!({})),
                "body": body,
                "truncated": truncated,
                "transport": "browser",
                "profile": profile,
                "target_id": page.target_id(),
            }));
        }
        if !fallback_direct {
            let err = browser_result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("browser request failed");
            return Err((INTERNAL_ERROR, err.to_string()));
        }
    }

    handle_browser_request_direct(params).await
}

async fn handle_browser_request_direct(params: &Value) -> RpcResult {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let method_raw = params
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let method = reqwest::Method::from_bytes(method_raw.as_bytes()).map_err(|e| {
        (
            INVALID_PARAMS,
            format!("invalid method '{method_raw}': {e}"),
        )
    })?;
    let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();

    // GET can use guarded redirect-following fetch with per-hop re-validation.
    let has_headers = params.get("headers").is_some();
    let has_body = params.get("body").is_some();
    if method == reqwest::Method::GET && !has_headers && !has_body {
        let resp = crate::ssrf::guarded_fetch(url, &ssrf_cfg)
            .await
            .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;
        let body = String::from_utf8_lossy(&resp.body).into_owned();
        let truncated = body.chars().count() > 100_000;
        let body = if truncated {
            body.chars().take(100_000).collect::<String>()
        } else {
            body
        };
        return Ok(json!({
            "status": resp.status.as_u16(),
            "url": resp.final_url,
            "content_type": resp.content_type,
            "body": body,
            "truncated": truncated,
        }));
    }

    crate::ssrf::validate_outbound_url(url, &ssrf_cfg)
        .await
        .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(ssrf_cfg.timeout_ms.max(1)))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| (INTERNAL_ERROR, format!("failed to create http client: {e}")))?;
    let mut req = client.request(method, url);

    if let Some(headers) = params.get("headers").and_then(|v| v.as_object()) {
        for (key, value) in headers {
            if let Some(v) = value.as_str() {
                req = req.header(key, v);
            }
        }
    }

    if let Some(body) = params.get("body") {
        if let Some(text) = body.as_str() {
            req = req.body(text.to_string());
        } else {
            req = req.json(body);
        }
    }

    let response = req
        .send()
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("request failed: {e}")))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("failed reading response body: {e}")))?;
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();
    let truncated = body_text.chars().count() > 100_000;
    let body = if truncated {
        body_text.chars().take(100_000).collect::<String>()
    } else {
        body_text
    };

    Ok(json!({
        "status": status,
        "url": url,
        "content_type": content_type,
        "location": location,
        "body": body,
        "truncated": truncated,
        "transport": "direct",
    }))
}

// ── Wizard ──────────────────────────────────────────────────────────────────

async fn handle_wizard_start(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let wizard_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("setup");
    wizard_store::start(&channel.config().savfox_home, wizard_type)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_wizard_next(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let wizard_id = params
        .get("wizard_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if wizard_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'wizard_id' parameter".to_string()));
    }
    let note = params.get("note").and_then(|v| v.as_str());
    wizard_store::next(&channel.config().savfox_home, wizard_id, note)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_wizard_cancel(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let wizard_id = params
        .get("wizard_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty());
    wizard_store::cancel(&channel.config().savfox_home, wizard_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

async fn handle_wizard_status(channel: &Arc<GatewayChannel>) -> RpcResult {
    wizard_store::status(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

// ── Misc ────────────────────────────────────────────────────────────────────

async fn handle_talk_mode(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("text");
    voice_store::set_talk_mode(&channel.config().savfox_home, mode)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_voicewake_get(channel: &Arc<GatewayChannel>) -> RpcResult {
    voice_store::get_voicewake(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_voicewake_set(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let keyword = params
        .get("keyword")
        .and_then(|v| v.as_str())
        .unwrap_or("hey savfox");
    voice_store::set_voicewake(&channel.config().savfox_home, enabled, keyword)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

async fn handle_update_run(_channel: &Arc<GatewayChannel>) -> RpcResult {
    Ok(json!({
        "status": "checking",
        "current_version": env!("CARGO_PKG_VERSION"),
        "checked_at": chrono::Utc::now().to_rfc3339(),
    }))
}

// ── Memory (Markdown 4-layer system) ────────────────────────────────────────

fn memory_home(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.clone()
}

fn memory_project_root() -> Option<std::path::PathBuf> {
    savfox_core::git_info::get_git_repo_root(&std::env::current_dir().unwrap_or_default())
}

async fn handle_memory_list(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let home = memory_home(channel);
    let pr = memory_project_root();
    let layer_filter = params.get("layer").and_then(|v| v.as_str());
    let include_content = params
        .get("include_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let entries =
        savfox_core::md_memory::discover_md_memories(&home, pr.as_deref(), "default").await;

    let filtered: Vec<_> = if let Some(layer_str) = layer_filter {
        let layer: savfox_core::md_memory::MemoryLayer = layer_str
            .parse()
            .map_err(|e: String| (INVALID_REQUEST, e))?;
        entries.into_iter().filter(|e| e.layer == layer).collect()
    } else {
        entries
    };

    let items: Vec<Value> = filtered
        .iter()
        .map(|e| {
            let mut obj = json!({
                "slug": e.slug,
                "layer": e.layer,
                "tags": e.frontmatter.tags,
                "priority": e.frontmatter.priority,
                "pinned": e.frontmatter.pinned,
                "author": e.frontmatter.author,
                "body_bytes": e.body_bytes,
            });
            if include_content {
                obj["body"] = Value::String(e.body.clone());
            }
            obj
        })
        .collect();

    Ok(json!({ "entries": items, "count": items.len() }))
}

async fn handle_memory_get(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(channel);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    for (layer, dir) in &dirs {
        if let Some(filter) = layer_filter {
            let filter_layer: savfox_core::md_memory::MemoryLayer =
                filter.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
            if *layer != filter_layer {
                continue;
            }
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;
            let (fm, body) = savfox_core::md_memory::parse_md_file(&content);
            return Ok(json!({
                "slug": slug,
                "layer": layer,
                "tags": fm.tags,
                "priority": fm.priority,
                "pinned": fm.pinned,
                "author": fm.author,
                "body": body,
                "raw": content,
            }));
        }
    }

    Err((INVALID_REQUEST, format!("memory entry '{slug}' not found")))
}

async fn handle_memory_create(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let layer_str = params
        .get("layer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'layer' parameter".to_string()))?;
    let layer: savfox_core::md_memory::MemoryLayer = layer_str
        .parse()
        .map_err(|e: String| (INVALID_REQUEST, e))?;

    if layer == savfox_core::md_memory::MemoryLayer::Session {
        return Err((
            INVALID_REQUEST,
            "session layer entries cannot be created on disk".to_string(),
        ));
    }

    let slug_raw = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let slug = savfox_core::md_memory::sanitize_slug(slug_raw);
    if !savfox_core::md_memory::is_valid_slug(&slug) {
        return Err((INVALID_REQUEST, format!("invalid slug '{slug_raw}'")));
    }

    let content = params
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'content' parameter".to_string()))?;

    let home = memory_home(channel);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");
    let dir = dirs
        .into_iter()
        .find(|(l, _)| *l == layer)
        .map(|(_, d)| d)
        .ok_or_else(|| (INVALID_REQUEST, format!("layer '{layer}' not available")))?;

    let path = dir.join(format!("{slug}.md"));
    if path.exists() {
        return Err((
            INVALID_REQUEST,
            format!("entry '{slug}' already exists in {layer} layer"),
        ));
    }

    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;

    let tags: Vec<String> = params
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let priority = params.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u32;

    let now = chrono::Utc::now();
    let fm = savfox_core::md_memory::MemoryFrontmatter {
        tags,
        priority,
        author: "user".to_string(),
        created_at: Some(now),
        updated_at: Some(now),
        ..Default::default()
    };

    let rendered = savfox_core::md_memory::render_md_file(&fm, content);
    tokio::fs::write(&path, &rendered)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "created", "slug": slug, "layer": layer_str }))
}

async fn handle_memory_update(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(channel);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    let mut found_path = None;
    for (layer, dir) in &dirs {
        if let Some(filter) = layer_filter {
            let filter_layer: savfox_core::md_memory::MemoryLayer =
                filter.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
            if *layer != filter_layer {
                continue;
            }
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            found_path = Some(path);
            break;
        }
    }

    let path = found_path.ok_or_else(|| (INVALID_REQUEST, format!("entry '{slug}' not found")))?;

    let existing = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;
    let (mut fm, old_body) = savfox_core::md_memory::parse_md_file(&existing);

    let new_body = params
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or(&old_body);

    if let Some(tags_arr) = params.get("tags").and_then(|v| v.as_array()) {
        fm.tags = tags_arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(p) = params.get("priority").and_then(|v| v.as_u64()) {
        fm.priority = p as u32;
    }
    fm.updated_at = Some(chrono::Utc::now());

    let rendered = savfox_core::md_memory::render_md_file(&fm, new_body);
    tokio::fs::write(&path, &rendered)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "updated", "slug": slug }))
}

async fn handle_memory_delete(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug' parameter".to_string()))?;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(channel);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    for (layer, dir) in &dirs {
        if let Some(filter) = layer_filter {
            let filter_layer: savfox_core::md_memory::MemoryLayer =
                filter.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
            if *layer != filter_layer {
                continue;
            }
        }
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("delete error: {e}")))?;
            return Ok(json!({ "status": "deleted", "slug": slug, "layer": layer.as_str() }));
        }
    }

    Err((INVALID_REQUEST, format!("entry '{slug}' not found")))
}

async fn handle_memory_search(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let layer_filter = params.get("layer").and_then(|v| v.as_str());

    let home = memory_home(channel);
    let pr = memory_project_root();
    let entries =
        savfox_core::md_memory::discover_md_memories(&home, pr.as_deref(), "default").await;

    let filtered: Vec<_> = if let Some(layer_str) = layer_filter {
        let layer: savfox_core::md_memory::MemoryLayer = layer_str
            .parse()
            .map_err(|e: String| (INVALID_REQUEST, e))?;
        entries.into_iter().filter(|e| e.layer == layer).collect()
    } else {
        entries
    };

    let results = savfox_core::md_memory::search_memories(&filtered, query, limit);
    let items: Vec<Value> = results
        .into_iter()
        .map(|e| {
            json!({
                "slug": e.slug,
                "layer": e.layer,
                "tags": e.frontmatter.tags,
                "priority": e.frontmatter.priority,
                "body_preview": if e.body.len() > 200 {
                    format!("{}...", &e.body[..200])
                } else {
                    e.body.clone()
                },
            })
        })
        .collect();

    Ok(json!({ "results": items, "count": items.len() }))
}

async fn handle_memory_promote(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let slug = params
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'slug'".to_string()))?;
    let from_str = params
        .get("from_layer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'from_layer'".to_string()))?;
    let to_str = params
        .get("to_layer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'to_layer'".to_string()))?;

    let from_layer: savfox_core::md_memory::MemoryLayer =
        from_str.parse().map_err(|e: String| (INVALID_REQUEST, e))?;
    let to_layer: savfox_core::md_memory::MemoryLayer =
        to_str.parse().map_err(|e: String| (INVALID_REQUEST, e))?;

    if to_layer == savfox_core::md_memory::MemoryLayer::Session {
        return Err((
            INVALID_REQUEST,
            "cannot promote to session layer".to_string(),
        ));
    }

    let home = memory_home(channel);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    // Find source.
    let source_dir = dirs
        .iter()
        .find(|(l, _)| *l == from_layer)
        .map(|(_, d)| d.clone())
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("from_layer '{from_str}' not available"),
            )
        })?;
    let source_path = source_dir.join(format!("{slug}.md"));
    if !source_path.exists() {
        return Err((
            INVALID_REQUEST,
            format!("entry '{slug}' not found in {from_str}"),
        ));
    }

    // Find target.
    let target_dir = dirs
        .iter()
        .find(|(l, _)| *l == to_layer)
        .map(|(_, d)| d.clone())
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("to_layer '{to_str}' not available"),
            )
        })?;

    let content = tokio::fs::read_to_string(&source_path)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;

    tokio::fs::create_dir_all(&target_dir)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;

    let target_path = target_dir.join(format!("{slug}.md"));
    tokio::fs::write(&target_path, &content)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    let _ = tokio::fs::remove_file(&source_path).await;

    Ok(json!({
        "status": "promoted",
        "slug": slug,
        "from": from_str,
        "to": to_str,
    }))
}

async fn handle_memory_layers(channel: &Arc<GatewayChannel>) -> RpcResult {
    let home = memory_home(channel);
    let pr = memory_project_root();
    let dirs = savfox_core::md_memory::layer_dirs(&home, pr.as_deref(), "default");

    let layers: Vec<Value> = dirs
        .iter()
        .map(|(layer, dir)| {
            json!({
                "layer": layer.as_str(),
                "path": dir.to_string_lossy(),
                "exists": dir.exists(),
            })
        })
        .collect();

    Ok(json!({ "layers": layers }))
}

// ─── Webhook handlers ───────────────────────────────────────────────────────

async fn handle_webhooks_list(channel: &GatewayChannel) -> RpcResult {
    let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
    store.load().await;
    let hooks = store.list().await;
    Ok(json!({ "webhooks": hooks }))
}

async fn handle_webhooks_get(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
    store.load().await;
    match store.get(id).await {
        Some(hook) => Ok(serde_json::to_value(hook).unwrap_or_default()),
        None => Err((INVALID_PARAMS, format!("webhook not found: {id}"))),
    }
}

async fn handle_webhooks_create(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let config: crate::webhooks::WebhookConfig = serde_json::from_value(params.clone())
        .map_err(|e| (INVALID_PARAMS, format!("invalid webhook config: {e}")))?;
    let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
    store.load().await;
    let hook = store
        .create(config)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "id": hook.id, "status": "created" }))
}

async fn handle_webhooks_update(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
    store.load().await;
    store
        .update(id, params.clone())
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "id": id, "status": "updated" }))
}

async fn handle_webhooks_delete(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
    store.load().await;
    store.delete(id).await.map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "id": id, "status": "deleted" }))
}

async fn handle_webhooks_test(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let id = params["id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing id".to_string()))?;
    let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
    store.load().await;
    let execs = store.recent_executions(Some(id), 5).await;
    Ok(json!({ "id": id, "recent_executions": execs.len() }))
}

// ─── Skill Registry handlers ────────────────────────────────────────────────

async fn handle_skills_registry_search(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let query = params["query"].as_str().unwrap_or("");
    let registry = crate::skill_registry::SkillRegistry::new(&channel.config().savfox_home, None);
    let results = registry
        .search(query)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "skills": results }))
}

async fn handle_skills_registry_install(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let name = params["name"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing name".to_string()))?;
    let registry = crate::skill_registry::SkillRegistry::new(&channel.config().savfox_home, None);
    let manifest = registry
        .get_manifest(name)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    let install_path = channel.config().savfox_home.join("skills").join(name);
    registry
        .record_install(manifest, install_path)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "name": name, "status": "installed" }))
}

async fn handle_skills_registry_uninstall(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let name = params["name"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing name".to_string()))?;
    let registry = crate::skill_registry::SkillRegistry::new(&channel.config().savfox_home, None);
    registry
        .record_uninstall(name)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "name": name, "status": "uninstalled" }))
}

// ─── DM Policy handlers ────────────────────────────────────────────────────

async fn handle_dm_policy_get(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let store = crate::dm_policy::DmPolicyStore::new(&channel.config().savfox_home);
    store.load().await;
    let policy = store.get_policy(channel).await;
    Ok(serde_json::to_value(policy).unwrap_or_default())
}

async fn handle_dm_policy_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let store = crate::dm_policy::DmPolicyStore::new(&channel.config().savfox_home);
    store.load().await;
    let policy: crate::dm_policy::ChannelDmPolicy =
        serde_json::from_value(params.clone()).unwrap_or_default();
    store.set_policy(channel.to_string(), policy).await;
    store.save().await.map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "channel": channel, "status": "updated" }))
}

async fn handle_dm_allowlist_get(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let store = crate::dm_policy::DmPolicyStore::new(&channel.config().savfox_home);
    store.load().await;
    let policy = store.get_policy(channel).await;
    Ok(json!({ "channel": channel, "allowlist": policy.allowlist }))
}

async fn handle_dm_allowlist_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let channel = params["channel"].as_str().unwrap_or("default");
    let entries = params["entries"]
        .as_array()
        .ok_or((INVALID_PARAMS, "missing entries array".to_string()))?;
    let store = crate::dm_policy::DmPolicyStore::new(&channel.config().savfox_home);
    store.load().await;
    for entry in entries {
        if let Some(sender) = entry.as_str() {
            store.allow_sender(channel, sender.to_string()).await;
        }
    }
    store.save().await.map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(json!({ "channel": channel, "status": "updated" }))
}

// ─── Provider Health handler ────────────────────────────────────────────────

async fn handle_providers_health(_channel: &GatewayChannel) -> RpcResult {
    let service = crate::provider_health::ProviderHealthService::new(300);
    let status = service.get_all().await;
    Ok(json!({ "providers": status }))
}

// ─── Config Reload / Validate / Migrate handlers ────────────────────────────

async fn handle_config_reload(channel: &GatewayChannel) -> RpcResult {
    let config_path = channel.config().savfox_home.join("config.json");
    let service = crate::config::reload::ConfigReloadService::new(config_path);

    // Load current config first so diff works
    let _ = service.load().await;

    // Reload from disk and compute diff
    let event = service
        .reload()
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("config reload failed: {e}")))?;

    // Validate the new config  - only block on actual errors, not warnings
    let new_config = service.get().await;
    let validation = crate::config::validator::validate_config(&new_config);
    let hard_errors: Vec<_> = validation
        .errors
        .iter()
        .filter(|e| matches!(e.severity, crate::config::validator::Severity::Error))
        .collect();
    if !hard_errors.is_empty() {
        return Err((
            INTERNAL_ERROR,
            format!(
                "config validation failed after reload: {}",
                hard_errors
                    .iter()
                    .map(|e| format!("{}: {}", e.field, e.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
    }

    Ok(json!({
        "status": "reloaded",
        "changed_keys": event.changed_keys,
        "timestamp": event.timestamp,
    }))
}

async fn handle_config_validate(params: &Value, _channel: &GatewayChannel) -> RpcResult {
    let result = crate::config::validator::validate_config(params);
    Ok(json!({ "valid": result.errors.is_empty(), "errors": result.errors }))
}

async fn handle_config_migrate(channel: &GatewayChannel) -> RpcResult {
    // Auto-snapshot before migration (#33)
    let _ = handle_config_snapshot(channel).await;

    let config_path = channel.config().savfox_home.join("config.json");
    if config_path.exists() {
        let data = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to read config: {e}")))?;
        let config: Value = serde_json::from_str(&data)
            .map_err(|e| (INTERNAL_ERROR, format!("failed to parse config: {e}")))?;
        let (migrated, changes) =
            crate::config::migrate::migrate(config).map_err(|e| (INTERNAL_ERROR, e))?;
        if !changes.is_empty() {
            let migrated_str = serde_json::to_string_pretty(&migrated)
                .map_err(|e| (INTERNAL_ERROR, format!("failed to serialize config: {e}")))?;
            tokio::fs::write(&config_path, migrated_str)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("failed to write config: {e}")))?;
        }
        Ok(json!({ "status": "migrated", "changes": changes }))
    } else {
        Ok(json!({ "status": "no_config_found" }))
    }
}

// ─── STT handlers ───────────────────────────────────────────────────────────

async fn handle_stt_transcribe(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let result = crate::stt::transcribe(&channel.config().savfox_home, channel.http_client(), params)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(result)
}

async fn handle_stt_providers() -> RpcResult {
    Ok(crate::stt::provider_info())
}

// ─── Agent Routing handlers ─────────────────────────────────────────────────

async fn handle_routing_rules_list(channel: &GatewayChannel) -> RpcResult {
    let path = channel.config().savfox_home.join("routing-rules.json");
    let rules: Vec<crate::agent_routing::RoutingRule> =
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("failed to read routing rules: {e}")))?;
            serde_json::from_str(&content)
                .map_err(|e| (INVALID_PARAMS, format!("invalid routing rules file: {e}")))?
        } else {
            Vec::new()
        };
    Ok(json!({ "rules": rules, "path": path }))
}

async fn handle_routing_rules_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let rules_value = params
        .get("rules")
        .cloned()
        .ok_or((INVALID_PARAMS, "missing rules array".to_string()))?;
    let rules: Vec<crate::agent_routing::RoutingRule> = serde_json::from_value(rules_value)
        .map_err(|e| (INVALID_PARAMS, format!("invalid rules payload: {e}")))?;
    let path = channel.config().savfox_home.join("routing-rules.json");
    let json = serde_json::to_string_pretty(&rules)
        .map_err(|e| (INTERNAL_ERROR, format!("failed to serialize rules: {e}")))?;
    tokio::fs::write(&path, json).await.map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to write routing rules: {e}"),
        )
    })?;
    Ok(json!({ "status": "updated", "count": rules.len(), "path": path }))
}

// ─── Canvas handlers ────────────────────────────────────────────────────────

use std::sync::LazyLock;

static CANVAS_SERVICE: LazyLock<crate::canvas_host::CanvasHostService> =
    LazyLock::new(crate::canvas_host::CanvasHostService::new);

async fn handle_canvas_create(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main").to_string();

    // Reset any existing state for this surface
    CANVAS_SERVICE.reset(&surface_id).await;

    Ok(json!({
        "surface_id": surface_id,
        "status": "created",
    }))
}

async fn handle_canvas_render(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main");

    let component: crate::canvas_host::A2UIComponent =
        serde_json::from_value(params["component"].clone())
            .map_err(|e| (INVALID_PARAMS, format!("invalid component: {e}")))?;

    CANVAS_SERVICE.push(surface_id, component).await;

    Ok(json!({
        "surface_id": surface_id,
        "status": "rendered",
    }))
}

async fn handle_canvas_action(params: &Value) -> RpcResult {
    let action: crate::canvas_host::A2UIAction = serde_json::from_value(params.clone())
        .map_err(|e| (INVALID_PARAMS, format!("invalid action: {e}")))?;

    let name = action.name.clone();
    let surface_id = action.surface_id.clone();
    CANVAS_SERVICE.handle_action(action).await;

    Ok(json!({
        "name": name,
        "surface_id": surface_id,
        "status": "dispatched",
    }))
}

async fn handle_canvas_state(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main");

    let state = CANVAS_SERVICE.get_state(surface_id).await;

    Ok(json!({
        "surface_id": surface_id,
        "state": state,
    }))
}

async fn handle_canvas_close(params: &Value) -> RpcResult {
    let surface_id = params["surface_id"].as_str().unwrap_or("main");

    CANVAS_SERVICE.reset(surface_id).await;

    Ok(json!({
        "surface_id": surface_id,
        "status": "closed",
    }))
}

// ── Plugins ─────────────────────────────────────────────────────────────────

fn plugin_registry() -> &'static Mutex<crate::plugin::PluginRegistry> {
    static REGISTRY: OnceLock<Mutex<crate::plugin::PluginRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(crate::plugin::PluginRegistry::new()))
}

async fn handle_plugins_list(channel: &GatewayChannel) -> RpcResult {
    let mut registry = plugin_registry().lock().await;

    // Discover plugins on first call
    let loader = crate::plugin::PluginLoader::new(&channel.config().savfox_home);
    let _ = loader.discover(&mut registry).await;

    let plugins: Vec<Value> = registry
        .list()
        .iter()
        .map(|p| {
            json!({
                "id": p.info.id,
                "name": p.info.name,
                "version": p.info.version,
                "description": p.info.description,
                "author": p.info.author,
                "state": format!("{:?}", p.state).to_lowercase(),
                "has_config": p.config_schema.is_some(),
            })
        })
        .collect();

    Ok(json!({ "plugins": plugins, "count": plugins.len() }))
}

async fn handle_plugins_enable(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let mut registry = plugin_registry().lock().await;
    registry.enable(id).map_err(|e| (INVALID_PARAMS, e))?;

    let loader = crate::plugin::PluginLoader::new(&channel.config().savfox_home);
    let _ = loader.save_state(&registry).await;

    Ok(json!({ "id": id, "status": "enabled" }))
}

async fn handle_plugins_disable(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let mut registry = plugin_registry().lock().await;
    registry.disable(id).map_err(|e| (INVALID_PARAMS, e))?;

    let loader = crate::plugin::PluginLoader::new(&channel.config().savfox_home);
    let _ = loader.save_state(&registry).await;

    Ok(json!({ "id": id, "status": "disabled" }))
}

async fn handle_plugins_config(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_string()));
    }

    let mut registry = plugin_registry().lock().await;

    // If config is provided, set it
    if let Some(config) = params.get("config") {
        registry
            .set_config(id, config.clone())
            .map_err(|e| (INVALID_PARAMS, e))?;

        let loader = crate::plugin::PluginLoader::new(&channel.config().savfox_home);
        let _ = loader.save_state(&registry).await;

        return Ok(json!({ "id": id, "status": "config_updated" }));
    }

    // Otherwise, return current config
    match registry.get(id) {
        Some(plugin) => Ok(json!({
            "id": id,
            "config": plugin.config,
            "config_schema": plugin.config_schema,
        })),
        None => Err((INVALID_PARAMS, format!("plugin not found: {id}"))),
    }
}

// ═══════════════════════════════════════════════════════════════════════════// P2 HANDLERS
// ═══════════════════════════════════════════════════════════════════════════
// ── Config Snapshots (#33) ──────────────────────────────────────────────────

async fn handle_config_snapshot(channel: &GatewayChannel) -> RpcResult {
    let config_path = channel.config().savfox_home.join("config.json");
    let backups_dir = channel.config().savfox_home.join("config-backups");

    if !backups_dir.exists() {
        tokio::fs::create_dir_all(&backups_dir)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .unwrap_or_else(|_| "{}".to_string());

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let filename = format!("{ts}.json");
    let snapshot_path = backups_dir.join(&filename);

    tokio::fs::write(&snapshot_path, &content)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "ok",
        "snapshot": filename,
        "timestamp": ts,
    }))
}

async fn handle_config_snapshots_list(channel: &GatewayChannel) -> RpcResult {
    let backups_dir = channel.config().savfox_home.join("config-backups");
    if !backups_dir.exists() {
        return Ok(json!({ "snapshots": [], "count": 0 }));
    }

    let mut snapshots = Vec::new();
    let mut entries = tokio::fs::read_dir(&backups_dir)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("readdir error: {e}")))?;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".json") {
            let meta = entry.metadata().await.ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            snapshots.push(json!({
                "name": name,
                "size": size,
            }));
        }
    }

    snapshots.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        bn.cmp(an)
    });

    let count = snapshots.len();
    Ok(json!({ "snapshots": snapshots, "count": count }))
}

async fn handle_config_restore(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let snapshot = params
        .get("snapshot")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if snapshot.is_empty() {
        return Err((INVALID_PARAMS, "missing 'snapshot' parameter".to_string()));
    }

    let backups_dir = channel.config().savfox_home.join("config-backups");
    let snapshot_path = backups_dir.join(snapshot);

    if !snapshot_path.exists() {
        return Err((INVALID_PARAMS, format!("snapshot not found: {snapshot}")));
    }

    let content = tokio::fs::read_to_string(&snapshot_path)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("read error: {e}")))?;

    // Auto-snapshot current config before restoring
    let _ = handle_config_snapshot(channel).await;

    let config_path = channel.config().savfox_home.join("config.json");
    tokio::fs::write(&config_path, &content)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "restored",
        "snapshot": snapshot,
    }))
}

// ── Model Aliases (#34) ─────────────────────────────────────────────────────

fn model_aliases_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("model-aliases.json")
}

async fn load_model_aliases(channel: &GatewayChannel) -> HashMap<String, String> {
    let path = model_aliases_path(channel);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

async fn save_model_aliases(
    channel: &GatewayChannel,
    aliases: &HashMap<String, String>,
) -> Result<(), String> {
    let path = model_aliases_path(channel);
    let json =
        serde_json::to_string_pretty(aliases).map_err(|e| format!("serialize error: {e}"))?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| format!("write error: {e}"))
}

async fn handle_models_aliases_get(channel: &GatewayChannel) -> RpcResult {
    let aliases = load_model_aliases(channel).await;
    Ok(json!({ "aliases": aliases }))
}

async fn handle_models_aliases_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let aliases_val = params
        .get("aliases")
        .ok_or_else(|| (INVALID_PARAMS, "missing 'aliases' parameter".to_string()))?;

    let aliases: HashMap<String, String> = serde_json::from_value(aliases_val.clone())
        .map_err(|e| (INVALID_PARAMS, format!("invalid aliases format: {e}")))?;

    save_model_aliases(channel, &aliases)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "ok", "count": aliases.len() }))
}

async fn handle_models_resolve(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if model.is_empty() {
        return Err((INVALID_PARAMS, "missing 'model' parameter".to_string()));
    }

    let aliases = load_model_aliases(channel).await;
    let resolved = aliases
        .get(model)
        .cloned()
        .unwrap_or_else(|| model.to_string());

    Ok(json!({
        "input": model,
        "resolved": resolved,
        "was_alias": resolved != model,
    }))
}

// ── Session Derived Titles (#35) ────────────────────────────────────────────

// Handled via sessions.patch  - title auto-generation on first message.
// The SessionEntry now has `title` and `derived_title` fields.
// When chat.send is called, if derived_title is None, generate from first 60 chars.

// ── Session Elevation (#46) ─────────────────────────────────────────────────

async fn handle_sessions_elevate(params: &Value, session_store: &SessionStore) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'session_id'".to_string()));
    }

    let timeout_mins = params
        .get("timeout_minutes")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let expires_at = now_ms + (timeout_mins * 60 * 1000);

    let result = session_store
        .update(session_id, |entry| {
            entry.elevated = true;
            entry.elevated_until = Some(expires_at);
            entry.touch();
        })
        .await;

    if result.is_some() {
        Ok(json!({
            "session_id": session_id,
            "elevated": true,
            "elevated_until": expires_at,
            "timeout_minutes": timeout_mins,
        }))
    } else {
        Err((INVALID_PARAMS, format!("session not found: {session_id}")))
    }
}

async fn handle_sessions_unelevate(params: &Value, session_store: &SessionStore) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'session_id'".to_string()));
    }

    let result = session_store
        .update(session_id, |entry| {
            entry.elevated = false;
            entry.elevated_until = None;
            entry.touch();
        })
        .await;

    if result.is_some() {
        Ok(json!({ "session_id": session_id, "elevated": false }))
    } else {
        Err((INVALID_PARAMS, format!("session not found: {session_id}")))
    }
}

// ── Heartbeat Config (#51) ──────────────────────────────────────────────────

fn heartbeat_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("heartbeat-config.json")
}

async fn handle_heartbeat_config_get(channel: &GatewayChannel) -> RpcResult {
    let path = heartbeat_config_path(channel);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_string());
    let config: Value = serde_json::from_str(&content).unwrap_or(json!({}));
    Ok(json!({ "config": config }))
}

async fn handle_heartbeat_config_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let config = params.get("config").cloned().unwrap_or(json!({}));
    let path = heartbeat_config_path(channel);
    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;
    Ok(json!({ "status": "ok" }))
}

// ── Browser CDP (#52) ───────────────────────────────────────────────────────

async fn browser_tab_summaries(
    profile: &str,
    timeout_ms: u64,
) -> Result<Vec<Value>, (i64, String)> {
    let browser = browser_session_browser(profile).await?;
    let active_target = active_browser_target(profile).await;
    let pages = with_browser_timeout(timeout_ms, "list browser tabs", browser.pages()).await?;
    let mut tabs = Vec::with_capacity(pages.len());
    for page in pages {
        let title = with_browser_timeout(timeout_ms, "read tab title", page.title())
            .await
            .ok();
        let url = with_browser_timeout(timeout_ms, "read tab url", page.url())
            .await
            .ok();
        tabs.push(json!({
            "target_id": page.target_id(),
            "title": title,
            "url": url,
            "active": active_target.as_deref() == Some(page.target_id()),
        }));
    }
    Ok(tabs)
}

async fn handle_browser_start(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
        if !url.trim().is_empty() {
            let page =
                select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
            with_browser_timeout(timeout_ms, "navigate", page.goto(url)).await?;
        }
    }
    let browser = browser_session_browser(&profile).await?;
    let tabs = browser_tab_summaries(&profile, timeout_ms).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "debug_port": browser.debug_port(),
        "tabs": tabs,
    }))
}

async fn handle_browser_stop(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let cfg = load_browser_profiles_config(channel).await;
    let profile = selected_profile_name(params, &cfg)?;
    let session = {
        let mut store = browser_runtime_store().lock().await;
        store.sessions.remove(&profile)
    };
    if let Some(runtime) = session {
        let _ = runtime.browser.close().await;
        Ok(json!({ "status": "ok", "profile": profile, "stopped": true }))
    } else {
        Ok(json!({ "status": "ok", "profile": profile, "stopped": false }))
    }
}

async fn handle_browser_tabs_list(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let tabs = browser_tab_summaries(&profile, timeout_ms).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "tabs": tabs,
    }))
}

async fn handle_browser_tabs_open(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let browser = browser_session_browser(&profile).await?;
    let page = with_browser_timeout(timeout_ms, "open tab", browser.new_page()).await?;
    let target_id = page.target_id().to_string();
    if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
        if !url.trim().is_empty() {
            let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
            crate::ssrf::validate_outbound_url(url, &ssrf_cfg)
                .await
                .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;
            with_browser_timeout(timeout_ms, "navigate", page.goto(url)).await?;
        }
    }
    set_active_browser_target(&profile, Some(target_id.clone())).await;
    let title = with_browser_timeout(timeout_ms, "read tab title", page.title())
        .await
        .ok();
    let url = with_browser_timeout(timeout_ms, "read tab url", page.url())
        .await
        .ok();
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": target_id,
        "title": title,
        "url": url,
    }))
}

async fn handle_browser_tabs_switch(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let target_id = params
        .get("target_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'target_id' parameter".to_string()))?
        .to_string();
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, Some(target_id)).await?;
    let title = with_browser_timeout(timeout_ms, "read tab title", page.title())
        .await
        .ok();
    let url = with_browser_timeout(timeout_ms, "read tab url", page.url())
        .await
        .ok();
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "title": title,
        "url": url,
    }))
}

async fn handle_browser_tabs_close(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let target_id = if let Some(id) = requested_target_id(params) {
        id
    } else if let Some(active) = active_browser_target(&profile).await {
        active
    } else {
        return Err((
            INVALID_PARAMS,
            "missing 'target_id' and no active tab".to_string(),
        ));
    };
    let page = select_browser_page(&profile, timeout_ms, Some(target_id.clone())).await?;
    with_browser_timeout(timeout_ms, "close tab", page.close()).await?;
    let browser = browser_session_browser(&profile).await?;
    let remaining = with_browser_timeout(timeout_ms, "list browser tabs", browser.pages())
        .await
        .unwrap_or_default();
    let next_active = remaining.first().map(|p| p.target_id().to_string());
    set_active_browser_target(&profile, next_active.clone()).await;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "closed_target_id": target_id,
        "active_target_id": next_active,
    }))
}

async fn handle_browser_snapshot(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("dom");
    let snapshot = match mode {
        "text" => with_browser_timeout(
            timeout_ms,
            "snapshot text",
            page.evaluate("(function(){ return document.body ? document.body.innerText : ''; })()"),
        )
        .await?
        .as_str()
        .unwrap_or_default()
        .to_string(),
        _ => with_browser_timeout(timeout_ms, "snapshot dom", page.content()).await?,
    };
    let max_chars = params
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(120_000) as usize;
    let truncated = snapshot.chars().count() > max_chars;
    let snapshot = if truncated {
        snapshot.chars().take(max_chars).collect::<String>()
    } else {
        snapshot
    };
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "mode": mode,
        "snapshot": snapshot,
        "truncated": truncated,
    }))
}

async fn handle_browser_storage_get(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("localStorage");
    let key = params.get("key").and_then(|v| v.as_str());
    let value = match scope {
        "cookies" => {
            let cookie = with_browser_timeout(
                timeout_ms,
                "read cookies",
                page.evaluate("document.cookie || ''"),
            )
            .await?
            .as_str()
            .unwrap_or_default()
            .to_string();
            if let Some(key) = key {
                let needle = format!("{key}=");
                json!(
                    cookie
                        .split(';')
                        .map(str::trim)
                        .find(|part| part.starts_with(&needle))
                        .map(|part| part[needle.len()..].to_string())
                )
            } else {
                json!(cookie)
            }
        }
        "sessionStorage" => {
            if let Some(key) = key {
                let key_json = serde_json::to_string(key)
                    .map_err(|e| (INTERNAL_ERROR, format!("invalid key: {e}")))?;
                let expr = format!("sessionStorage.getItem({key_json})");
                with_browser_timeout(timeout_ms, "read sessionStorage key", page.evaluate(&expr))
                    .await?
            } else {
                with_browser_timeout(
                    timeout_ms,
                    "read sessionStorage",
                    page.evaluate(
                        "(function(){return Object.fromEntries(Object.keys(sessionStorage).map(k => [k, sessionStorage.getItem(k)]));})()",
                    ),
                )
                .await?
            }
        }
        _ => {
            if let Some(key) = key {
                let key_json = serde_json::to_string(key)
                    .map_err(|e| (INTERNAL_ERROR, format!("invalid key: {e}")))?;
                let expr = format!("localStorage.getItem({key_json})");
                with_browser_timeout(timeout_ms, "read localStorage key", page.evaluate(&expr))
                    .await?
            } else {
                with_browser_timeout(
                    timeout_ms,
                    "read localStorage",
                    page.evaluate(
                        "(function(){return Object.fromEntries(Object.keys(localStorage).map(k => [k, localStorage.getItem(k)]));})()",
                    ),
                )
                .await?
            }
        }
    };
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "scope": scope,
        "key": key,
        "value": value,
    }))
}

async fn handle_browser_storage_set(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("localStorage");
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'key' parameter".to_string()))?;
    let value = params
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'value' parameter".to_string()))?;
    let key_json =
        serde_json::to_string(key).map_err(|e| (INTERNAL_ERROR, format!("invalid key: {e}")))?;
    let value_json = serde_json::to_string(value)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid value: {e}")))?;
    let expr = match scope {
        "cookies" => format!(
            r#"(function() {{ document.cookie = {key_json} + "=" + encodeURIComponent({value_json}) + ";path=/"; return true; }})()"#
        ),
        "sessionStorage" => format!(
            r#"(function() {{ sessionStorage.setItem({key_json}, {value_json}); return true; }})()"#
        ),
        _ => format!(
            r#"(function() {{ localStorage.setItem({key_json}, {value_json}); return true; }})()"#
        ),
    };
    with_browser_timeout(timeout_ms, "set browser storage", page.evaluate(&expr)).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "scope": scope,
        "key": key,
    }))
}

async fn handle_browser_storage_clear(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("localStorage");
    let expr = match scope {
        "cookies" => {
            r#"(function(){
                const cookies = document.cookie.split(";");
                for (const cookie of cookies) {
                    const eq = cookie.indexOf("=");
                    const name = (eq > -1 ? cookie.slice(0, eq) : cookie).trim();
                    if (name) {
                        document.cookie = name + "=;expires=Thu, 01 Jan 1970 00:00:00 GMT;path=/";
                    }
                }
                return true;
            })()"#
        }
        "sessionStorage" => r#"(function(){ sessionStorage.clear(); return true; })()"#,
        _ => r#"(function(){ localStorage.clear(); return true; })()"#,
    };
    with_browser_timeout(timeout_ms, "clear browser storage", page.evaluate(expr)).await?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "scope": scope,
    }))
}

async fn handle_browser_download(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let selector = params
        .get("selector")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if selector.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'selector' parameter".to_string()));
    }

    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let target_id = page.target_id().to_string();

    let download_dir = params
        .get("download_dir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            channel
                .config()
                .savfox_home
                .join("browser")
                .join("downloads")
                .join(&profile)
        });
    tokio::fs::create_dir_all(&download_dir)
        .await
        .map_err(|e| {
            (
                INTERNAL_ERROR,
                format!(
                    "failed to create download dir {}: {e}",
                    download_dir.display()
                ),
            )
        })?;

    let event = with_browser_timeout(
        timeout_ms,
        "capture browser download",
        page.click_and_wait_for_download(selector, &download_dir, timeout_ms),
    )
    .await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": target_id,
        "selector": selector,
        "download": event,
    }))
}

async fn handle_browser_network_capture(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let duration_ms = params
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(2_000)
        .max(1);
    let max_events = params
        .get("max_events")
        .and_then(|v| v.as_u64())
        .unwrap_or(128)
        .max(1) as usize;
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let target_id = page.target_id().to_string();

    let responses = with_browser_timeout(
        timeout_ms.saturating_add(duration_ms).saturating_add(500),
        "capture browser network responses",
        page.capture_responses(duration_ms, max_events),
    )
    .await?;
    let responses_value = serde_json::to_value(&responses).unwrap_or_else(|_| json!([]));

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": target_id,
        "duration_ms": duration_ms,
        "count": responses.len(),
        "responses": responses_value,
    }))
}

async fn handle_browser_profiles_list(channel: &Arc<GatewayChannel>) -> RpcResult {
    let cfg = load_browser_profiles_config(channel).await;
    let running_profiles: HashSet<String> = {
        let store = browser_runtime_store().lock().await;
        store.sessions.keys().cloned().collect()
    };
    let mut names: HashSet<String> = cfg.profiles.keys().cloned().collect();
    names.insert(cfg.default_profile.clone());
    if names.is_empty() {
        names.insert(default_browser_profile_name());
    }
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort();
    let mut profiles = Vec::with_capacity(names.len());
    for name in names {
        let settings = cfg.profiles.get(&name).cloned().unwrap_or_default();
        let effective_user_data_dir = settings.user_data_dir.clone().unwrap_or_else(|| {
            browser_profile_default_dir(channel, &name)
                .display()
                .to_string()
        });
        profiles.push(json!({
            "name": name,
            "default": name == cfg.default_profile,
            "running": running_profiles.contains(&name),
            "settings": {
                "user_data_dir": effective_user_data_dir,
                "executable_path": settings.executable_path,
                "extensions": settings.extensions,
                "args": settings.args,
                "proxy": settings.proxy,
                "headless": settings.headless,
            }
        }));
    }
    Ok(json!({
        "default_profile": cfg.default_profile,
        "profiles": profiles,
    }))
}

async fn handle_browser_profiles_create(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let requested = params
        .get("profile")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'profile' parameter".to_string()))?;
    let profile = validate_browser_profile_name(requested)?;
    let mut cfg = load_browser_profiles_config(channel).await;
    let current = cfg.profiles.get(&profile).cloned().unwrap_or_default();
    let merged = browser_profile_settings_from_params(params, current);
    cfg.profiles.insert(profile.clone(), merged.clone());
    if params
        .get("default")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || cfg.default_profile.trim().is_empty()
    {
        cfg.default_profile = profile.clone();
    }
    save_browser_profiles_config(channel, &cfg).await?;
    let data_dir = merged
        .user_data_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| browser_profile_default_dir(channel, &profile));
    tokio::fs::create_dir_all(&data_dir).await.map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("failed to create profile data dir: {e}"),
        )
    })?;
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "default_profile": cfg.default_profile,
        "settings": {
            "user_data_dir": data_dir.display().to_string(),
            "executable_path": merged.executable_path,
            "extensions": merged.extensions,
            "args": merged.args,
            "proxy": merged.proxy,
            "headless": merged.headless,
        }
    }))
}

async fn handle_browser_profiles_delete(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let requested = params
        .get("profile")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'profile' parameter".to_string()))?;
    let profile = validate_browser_profile_name(requested)?;
    let mut cfg = load_browser_profiles_config(channel).await;
    if cfg.profiles.remove(&profile).is_none() {
        return Err((
            INVALID_PARAMS,
            format!("profile '{profile}' does not exist"),
        ));
    }
    if cfg.default_profile == profile {
        cfg.default_profile = cfg
            .profiles
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(default_browser_profile_name);
    }
    save_browser_profiles_config(channel, &cfg).await?;
    let session = {
        let mut store = browser_runtime_store().lock().await;
        store.sessions.remove(&profile)
    };
    if let Some(runtime) = session {
        let _ = runtime.browser.close().await;
    }
    if params
        .get("delete_data")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let path = browser_profile_default_dir(channel, &profile);
        let _ = tokio::fs::remove_dir_all(path).await;
    }
    Ok(json!({
        "status": "ok",
        "profile": profile,
        "default_profile": cfg.default_profile,
    }))
}

async fn handle_browser_profiles_default_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let requested = params
        .get("profile")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (INVALID_PARAMS, "missing 'profile' parameter".to_string()))?;
    let profile = validate_browser_profile_name(requested)?;
    let mut cfg = load_browser_profiles_config(channel).await;
    cfg.profiles.entry(profile.clone()).or_default();
    cfg.default_profile = profile.clone();
    save_browser_profiles_config(channel, &cfg).await?;
    Ok(json!({
        "status": "ok",
        "default_profile": profile,
    }))
}

async fn handle_browser_goto(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        return Err((INVALID_PARAMS, "missing 'url' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;

    let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
    crate::ssrf::validate_outbound_url(url, &ssrf_cfg)
        .await
        .map_err(|e| (INVALID_PARAMS, format!("blocked URL: {e}")))?;

    let open_new_tab = params
        .get("open_new_tab")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let page = if open_new_tab {
        let browser = browser_session_browser(&profile).await?;
        let page = with_browser_timeout(timeout_ms, "open tab", browser.new_page()).await?;
        set_active_browser_target(&profile, Some(page.target_id().to_string())).await;
        page
    } else {
        select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?
    };
    with_browser_timeout(timeout_ms, "navigate", page.goto(url)).await?;
    let title = with_browser_timeout(timeout_ms, "read page title", page.title())
        .await
        .ok();

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "goto",
        "target_id": page.target_id(),
        "url": url,
        "title": title,
    }))
}

async fn handle_browser_click(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let selector = params
        .get("selector")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if selector.is_empty() {
        return Err((INVALID_PARAMS, "missing 'selector' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    if let Some(xpath) = is_xpath_selector(selector) {
        let xpath_json = serde_json::to_string(xpath)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid xpath selector: {e}")))?;
        let expr = format!(
            r#"(function() {{
                const node = document.evaluate({xpath_json}, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
                if (!node) return false;
                if (typeof node.scrollIntoView === "function") node.scrollIntoView({{ block: "center" }});
                if (typeof node.click === "function") node.click();
                return true;
            }})()"#
        );
        let ok = with_browser_timeout(timeout_ms, "click xpath element", page.evaluate(&expr))
            .await?
            .as_bool()
            .unwrap_or(false);
        if !ok {
            return Err((
                INVALID_PARAMS,
                format!("element not found for xpath '{xpath}'"),
            ));
        }
    } else {
        with_browser_timeout(timeout_ms, "click element", page.click(selector)).await?;
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "click",
        "target_id": page.target_id(),
        "selector": selector,
    }))
}

async fn handle_browser_type(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let selector = params
        .get("selector")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if selector.is_empty() {
        return Err((INVALID_PARAMS, "missing 'selector' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("type");
    if !matches!(mode, "type" | "fill" | "select") {
        return Err((
            INVALID_PARAMS,
            "invalid 'mode' parameter (expected type/fill/select)".to_string(),
        ));
    }
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let xpath = is_xpath_selector(selector);
    if mode == "type" && xpath.is_none() {
        with_browser_timeout(timeout_ms, "type text", page.type_text(selector, text)).await?;
    } else {
        let selector_raw = xpath.unwrap_or(selector);
        let selector_json = serde_json::to_string(selector_raw)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid selector: {e}")))?;
        let text_json = serde_json::to_string(text)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid text: {e}")))?;
        let mode_json = serde_json::to_string(mode)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid mode: {e}")))?;
        let is_xpath = xpath.is_some();
        let expr = format!(
            r#"(function() {{
                const selector = {selector_json};
                const text = {text_json};
                const mode = {mode_json};
                const isXPath = {};
                let el = null;
                if (isXPath) {{
                    el = document.evaluate(selector, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
                }} else {{
                    el = document.querySelector(selector);
                }}
                if (!el) return {{ ok: false, error: "element not found" }};
                if (typeof el.focus === "function") el.focus();
                if (mode === "select") {{
                    el.value = text;
                    el.dispatchEvent(new Event("input", {{ bubbles: true }}));
                    el.dispatchEvent(new Event("change", {{ bubbles: true }}));
                    return {{ ok: true }};
                }}
                el.value = text;
                el.dispatchEvent(new Event("input", {{ bubbles: true }}));
                el.dispatchEvent(new Event("change", {{ bubbles: true }}));
                return {{ ok: true }};
            }})()"#,
            is_xpath
        );
        let result =
            with_browser_timeout(timeout_ms, "fill form field", page.evaluate(&expr)).await?;
        if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let err = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("form update failed");
            return Err((INTERNAL_ERROR, err.to_string()));
        }
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "type",
        "mode": mode,
        "target_id": page.target_id(),
        "selector": selector,
        "text": text,
    }))
}

async fn handle_browser_screenshot(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let full_page = params
        .get("full_page")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let selector = params.get("selector").and_then(|v| v.as_str());
    let mut options = ScreenshotOptions::new();
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    options = options.format(match format.as_str() {
        "jpeg" | "jpg" => ScreenshotFormat::Jpeg,
        "webp" => ScreenshotFormat::Webp,
        "png" => ScreenshotFormat::Png,
        _ => {
            return Err((
                INVALID_PARAMS,
                "invalid screenshot format (expected png/jpeg/webp)".to_string(),
            ));
        }
    });
    if let Some(quality) = params.get("quality").and_then(|v| v.as_u64()) {
        if quality > 100 {
            return Err((INVALID_PARAMS, "quality must be 0-100".to_string()));
        }
        options = options.quality(quality as u8);
    }

    if let Some(selector) = selector {
        let selector_raw = is_xpath_selector(selector).unwrap_or(selector);
        let selector_json = serde_json::to_string(selector_raw)
            .map_err(|e| (INTERNAL_ERROR, format!("invalid selector: {e}")))?;
        let is_xpath = is_xpath_selector(selector).is_some();
        let expr = format!(
            r#"(function() {{
                const selector = {selector_json};
                const isXPath = {};
                let el = null;
                if (isXPath) {{
                    el = document.evaluate(selector, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
                }} else {{
                    el = document.querySelector(selector);
                }}
                if (!el) return null;
                const r = el.getBoundingClientRect();
                return {{
                    x: Math.max(0, r.left + window.scrollX),
                    y: Math.max(0, r.top + window.scrollY),
                    width: Math.max(1, r.width),
                    height: Math.max(1, r.height)
                }};
            }})()"#,
            is_xpath
        );
        let rect =
            with_browser_timeout(timeout_ms, "resolve element rect", page.evaluate(&expr)).await?;
        let x = rect.get("x").and_then(|v| v.as_f64());
        let y = rect.get("y").and_then(|v| v.as_f64());
        let width = rect.get("width").and_then(|v| v.as_f64());
        let height = rect.get("height").and_then(|v| v.as_f64());
        if let (Some(x), Some(y), Some(width), Some(height)) = (x, y, width, height) {
            options = options.clip(x, y, width, height);
        } else {
            return Err((
                INVALID_PARAMS,
                format!("element not found for selector '{selector}'"),
            ));
        }
    } else if full_page {
        let dims = with_browser_timeout(
            timeout_ms,
            "resolve full-page dimensions",
            page.evaluate(
                "(function(){return { width: Math.max(document.documentElement.scrollWidth, document.body ? document.body.scrollWidth : 0, window.innerWidth), height: Math.max(document.documentElement.scrollHeight, document.body ? document.body.scrollHeight : 0, window.innerHeight) };})()",
            ),
        )
        .await?;
        let width = dims.get("width").and_then(|v| v.as_f64()).unwrap_or(1280.0);
        let height = dims.get("height").and_then(|v| v.as_f64()).unwrap_or(720.0);
        options = options.full_page(width.max(1.0), height.max(1.0));
    } else if let Some(clip) = params.get("clip").and_then(|v| v.as_object()) {
        let x = clip.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = clip.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let width = clip.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let height = clip.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if width <= 0.0 || height <= 0.0 {
            return Err((
                INVALID_PARAMS,
                "clip.width/clip.height must be > 0".to_string(),
            ));
        }
        options = options.clip(x, y, width, height);
    }

    let bytes =
        with_browser_timeout(timeout_ms, "capture screenshot", page.screenshot(options)).await?;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "screenshot",
        "target_id": page.target_id(),
        "full_page": full_page,
        "selector": selector,
        "format": format,
        "bytes": bytes.len(),
        "image_base64": encoded,
    }))
}

async fn handle_browser_eval(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let expression = params
        .get("expression")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if expression.is_empty() {
        return Err((INVALID_PARAMS, "missing 'expression' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;
    let result =
        with_browser_timeout(timeout_ms, "evaluate javascript", page.evaluate(expression)).await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "action": "eval",
        "target_id": page.target_id(),
        "expression": expression,
        "result": result,
    }))
}

fn browser_extension_relay_bootstrap_expr(channel: &str) -> Result<String, (i64, String)> {
    let channel_json = serde_json::to_string(channel)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay channel: {e}")))?;
    Ok(format!(
        r#"(function() {{
            const channel = {channel_json};
            if (!window.__savfoxRelay || window.__savfoxRelay.channel !== channel) {{
                window.__savfoxRelay = {{ channel, queue: [] }};
                window.addEventListener("message", function(event) {{
                    const data = event && event.data;
                    if (!data || typeof data !== "object") return;
                    if (data.__savfoxRelay !== true) return;
                    if (data.channel && data.channel !== window.__savfoxRelay.channel) return;
                    window.__savfoxRelay.queue.push({{
                        type: typeof data.type === "string" ? data.type : "message",
                        payload: Object.prototype.hasOwnProperty.call(data, "payload") ? data.payload : null,
                        ts: Date.now(),
                        origin: event.origin || null
                    }});
                    if (window.__savfoxRelay.queue.length > 512) {{
                        const trim = window.__savfoxRelay.queue.length - 512;
                        window.__savfoxRelay.queue.splice(0, trim);
                    }}
                }});
            }}
            return {{ ok: true, channel: window.__savfoxRelay.channel }};
        }})()"#
    ))
}

async fn handle_browser_extension_relay_start(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .trim()
        .to_string();
    if channel.is_empty() {
        return Err((INVALID_PARAMS, "relay channel cannot be empty".to_string()));
    }

    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = browser_extension_relay_bootstrap_expr(&channel)?;
    let result =
        with_browser_timeout(timeout_ms, "start extension relay", page.evaluate(&expr)).await?;
    let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Err((
            INTERNAL_ERROR,
            "failed to start extension relay".to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "channel": channel,
    }))
}

async fn handle_browser_extension_relay_status(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = r#"(function() {
        const relay = window.__savfoxRelay;
        if (!relay || !Array.isArray(relay.queue)) {
            return { started: false, channel: null, queued: 0 };
        }
        return {
            started: true,
            channel: relay.channel || null,
            queued: relay.queue.length
        };
    })()"#;
    let result = with_browser_timeout(
        timeout_ms,
        "query extension relay status",
        page.evaluate(expr),
    )
    .await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "relay": result,
    }))
}

async fn handle_browser_extension_relay_stop(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = r#"(function() {
        const relay = window.__savfoxRelay;
        if (!relay || !Array.isArray(relay.queue)) {
            return { stopped: false, reason: "relay_not_started" };
        }
        const channel = relay.channel || null;
        const queued = relay.queue.length;
        relay.queue.splice(0, relay.queue.length);
        delete window.__savfoxRelay;
        return { stopped: true, channel, queued };
    })()"#;
    let result =
        with_browser_timeout(timeout_ms, "stop extension relay", page.evaluate(expr)).await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "relay": result,
    }))
}

async fn handle_browser_extension_relay_poll(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = r#"(function() {
        const relay = window.__savfoxRelay;
        if (!relay || !Array.isArray(relay.queue)) {
            return { ok: false, reason: "relay_not_started", channel: null, messages: [] };
        }
        const messages = relay.queue.splice(0, relay.queue.length);
        return { ok: true, channel: relay.channel || null, messages };
    })()"#;
    let result =
        with_browser_timeout(timeout_ms, "poll extension relay", page.evaluate(expr)).await?;

    Ok(json!({
        "status": if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) { "ok" } else { "empty" },
        "profile": profile,
        "target_id": page.target_id(),
        "channel": result.get("channel").cloned().unwrap_or(Value::Null),
        "messages": result.get("messages").cloned().unwrap_or_else(|| json!([])),
        "reason": result.get("reason").cloned().unwrap_or(Value::Null),
    }))
}

async fn handle_browser_extension_relay_send(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let event_type = params
        .get("event_type")
        .or_else(|| params.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("message")
        .to_string();
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);

    let channel_json = serde_json::to_string(&channel)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay channel: {e}")))?;
    let event_type_json = serde_json::to_string(&event_type)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay event type: {e}")))?;
    let payload_json = serde_json::to_string(&payload)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid relay payload: {e}")))?;
    let expr = format!(
        r#"(function() {{
            const relay = window.__savfoxRelay;
            if (!relay) return {{ ok: false, reason: "relay_not_started" }};
            const detail = {{
                __savfoxRelay: true,
                channel: {channel_json},
                type: {event_type_json},
                payload: {payload_json},
                ts: Date.now()
            }};
            window.dispatchEvent(new CustomEvent("savfox-relay", {{ detail }}));
            return {{ ok: true, channel: detail.channel, type: detail.type }};
        }})()"#
    );
    let result = with_browser_timeout(
        timeout_ms,
        "send extension relay message",
        page.evaluate(&expr),
    )
    .await?;
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err((
            INVALID_PARAMS,
            result
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("relay_not_started")
                .to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "channel": channel,
        "type": event_type,
    }))
}

async fn handle_browser_content_script_inject(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let script = params.get("script").and_then(|v| v.as_str()).unwrap_or("");
    if script.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'script' parameter".to_string()));
    }
    let timeout_ms = browser_timeout_ms(params);
    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let script_json = serde_json::to_string(script)
        .map_err(|e| (INTERNAL_ERROR, format!("invalid content script: {e}")))?;
    let expr = format!(
        r#"(function() {{
            const source = {script_json};
            try {{
                const fn = new Function(source);
                const result = fn();
                return {{ ok: true, result: result === undefined ? null : result }};
            }} catch (err) {{
                return {{ ok: false, error: String(err) }};
            }}
        }})()"#
    );
    let result =
        with_browser_timeout(timeout_ms, "inject content script", page.evaluate(&expr)).await?;
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err((
            INTERNAL_ERROR,
            result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("script injection failed")
                .to_string(),
        ));
    }

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "result": result.get("result").cloned().unwrap_or(Value::Null),
    }))
}

async fn handle_browser_page_extract(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let timeout_ms = browser_timeout_ms(params);
    let interactive_only = params
        .get("interactive_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_interactive = params
        .get("max_interactive")
        .and_then(|v| v.as_u64())
        .unwrap_or(128)
        .max(1) as usize;
    let max_text_chars = params
        .get("max_text_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(8_000)
        .max(128) as usize;
    let max_links = params
        .get("max_links")
        .and_then(|v| v.as_u64())
        .unwrap_or(64)
        .max(1) as usize;
    let include_html = params
        .get("include_html")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_html_chars = params
        .get("max_html_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(40_000)
        .max(256) as usize;

    let (profile, settings) = resolve_browser_profile(params, channel).await?;
    ensure_browser_session_for_profile(channel, &profile, &settings).await?;
    let page = select_browser_page(&profile, timeout_ms, requested_target_id(params)).await?;

    let expr = format!(
        r#"(function() {{
            const maxText = {max_text_chars};
            const maxLinks = {max_links};
            const includeHtml = {include_html};
            const maxHtml = {max_html_chars};
            const interactiveOnly = {interactive_only};
            const maxInteractive = {max_interactive};
            const text = interactiveOnly ? "" : ((document.body && document.body.innerText) || "").slice(0, maxText);
            const headings = interactiveOnly ? [] : Array.from(document.querySelectorAll("h1, h2, h3")).slice(0, 32).map(el => {{
                const level = Number((el.tagName || "H1").slice(1)) || 1;
                return {{ level, text: (el.innerText || "").trim() }};
            }});
            const links = Array.from(document.querySelectorAll("a[href]")).slice(0, maxLinks).map(el => {{
                return {{
                    href: el.href || "",
                    text: (el.textContent || "").trim().slice(0, 256)
                }};
            }});
            const metas = interactiveOnly ? [] : Array.from(document.querySelectorAll("meta[name], meta[property]")).slice(0, 32).map(meta => {{
                return {{
                    key: meta.getAttribute("name") || meta.getAttribute("property") || "",
                    value: meta.getAttribute("content") || ""
                }};
            }});

            const getSelector = (el) => {{
                if (!el || !el.tagName) return "";
                const tag = el.tagName.toLowerCase();
                const id = el.id ? `#${{el.id}}` : "";
                if (id) return `${{tag}}${{id}}`;
                const name = el.getAttribute("name");
                if (name) return `${{tag}}[name="${{name}}"]`;
                if (el.classList && el.classList.length > 0) {{
                    const cls = Array.from(el.classList).slice(0, 2).join(".");
                    if (cls) return `${{tag}}.${{cls}}`;
                }}
                return tag;
            }};
            const interactiveElements = Array.from(
                document.querySelectorAll('a[href],button,input,textarea,select,[role="button"],[onclick],[tabindex]')
            )
                .filter((el) => {{
                    if (!el || !(el instanceof Element)) return false;
                    const style = window.getComputedStyle(el);
                    if (!style) return false;
                    if (style.display === "none" || style.visibility === "hidden") return false;
                    const rect = el.getBoundingClientRect();
                    return rect.width > 0 && rect.height > 0;
                }})
                .slice(0, maxInteractive)
                .map((el) => {{
                    const tag = (el.tagName || "").toLowerCase();
                    const role = el.getAttribute("role") || null;
                    const label = (
                        el.getAttribute("aria-label") ||
                        el.getAttribute("title") ||
                        el.getAttribute("placeholder") ||
                        el.textContent ||
                        ""
                    ).trim().slice(0, 120);
                    return {{
                        tag,
                        role,
                        selector: getSelector(el),
                        type: el.getAttribute("type") || null,
                        href: el.getAttribute("href") || null,
                        label
                    }};
                }});
            const html = includeHtml
                ? ((document.documentElement && document.documentElement.outerHTML) || "").slice(0, maxHtml)
                : null;
            return {{
                url: location.href,
                title: document.title || "",
                lang: document.documentElement ? (document.documentElement.lang || "") : "",
                mode: interactiveOnly ? "interactive" : "full",
                text,
                text_chars: text.length,
                headings,
                links,
                interactive_elements: interactiveElements,
                meta: metas,
                html,
                html_truncated: includeHtml && ((document.documentElement && document.documentElement.outerHTML) || "").length > maxHtml
            }};
        }})()"#
    );
    let result =
        with_browser_timeout(timeout_ms, "extract page data", page.evaluate(&expr)).await?;

    Ok(json!({
        "status": "ok",
        "profile": profile,
        "target_id": page.target_id(),
        "data": result,
    }))
}


use super::*;

async fn edge(home: &Path, seed: u8) -> Arc<ArkretEdge> {
    let mut config = super::super::tests::valid_channel_config();
    config.id = format!("manager-host-{seed}");
    let now = chrono::Utc::now();
    let mut identity = arkret_test_kit::service_identity::ServiceIdentityFixture::webvh(
        &format!("https://host-{seed}.example/service/")
            .parse()
            .unwrap(),
        812,
        [seed; 32],
        "key-1",
        now - chrono::Duration::minutes(1),
        now,
    )
    .unwrap();
    identity.resolution.service_kind = "applet_service".into();
    let resolution = home.join(format!("resolution-{seed}.json"));
    std::fs::write(
        &resolution,
        serde_json::to_vec(&identity.resolution).unwrap(),
    )
    .unwrap();
    let service_key = home.join(format!("service-{seed}.key"));
    std::fs::write(&service_key, [seed; 32]).unwrap();
    config.config["serviceId"] = serde_json::json!(identity.service_id);
    config.config["service_did"] =
        serde_json::json!(identity.resolution.normalized_did_document.id);
    config.config["verification_method"] = serde_json::json!(format!(
        "{}#key-1",
        identity.resolution.normalized_did_document.id
    ));
    config.config["service_resolution_path"] = serde_json::json!(resolution);
    config.config["keyRef"] = serde_json::json!({"kind":"file","path":service_key});
    let config =
        savfox_channels::arkret::applet::ArkretAppletConfig::from_channel_config(&config).unwrap();
    let journal = super::super::build_applet_authoring_journal(home, &config.id).unwrap();
    Arc::new(
        super::super::build_applet_edge(&config, journal)
            .await
            .unwrap(),
    )
}

fn provision(root: &Path, edge: &ArkretEdge, key: u8) -> PathBuf {
    let directory = owner_directory(&root.canonicalize().unwrap(), edge).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("wrapping.key"), [key; 32]).unwrap();
    directory
}

async fn describe(host: &ManagerHost) -> arkret::ServiceDescribe {
    let mut headers = salvo::http::HeaderMap::new();
    headers.insert(
        "arkret-operation",
        arkret::ServiceOperationId::SERVER_READ_DESCRIBE_V1
            .parse()
            .unwrap(),
    );
    let (status, _, body) = host
        .receive(
            salvo::http::Method::GET,
            "https://savfox.example/_arkret/describe".parse().unwrap(),
            headers,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(status, salvo::http::StatusCode::OK);
    let description: arkret::ServiceDescribe = serde_json::from_slice(&body).unwrap();
    description.validate().unwrap();
    description
}

async fn wait_closed(path: &Path) {
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if probe.try_lock().is_ok() {
            probe.unlock().unwrap();
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "manager vault remained open"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn multiple_owners_use_distinct_real_vaults_and_real_management_hosts() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let first = edge(home.path(), 61).await;
    let second = edge(home.path(), 62).await;
    let first_dir = provision(root.path(), &first, 81);
    let second_dir = provision(root.path(), &second, 82);
    assert_ne!(first_dir, second_dir);
    let first = ManagerHost::from_root(root.path(), first).unwrap();
    let second = ManagerHost::from_root(root.path(), second).unwrap();
    assert!(!Arc::ptr_eq(
        &first._deployment.vault,
        &second._deployment.vault
    ));
    assert_ne!(
        describe(&first).await.service_resolution.did,
        describe(&second).await.service_resolution.did
    );
    assert!(first_dir.join("archive.redb").is_file());
    assert!(second_dir.join("archive.redb").is_file());
}

#[tokio::test]
async fn same_owner_refresh_reuses_vault_and_wrong_key_leaves_original_host_usable() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let edge = edge(home.path(), 63).await;
    let owner = provision(root.path(), &edge, 83);
    let first = ManagerHost::from_root(root.path(), edge.clone()).unwrap();
    let second = ManagerHost::from_root(root.path(), edge.clone()).unwrap();
    assert!(Arc::ptr_eq(
        &first._deployment.vault,
        &second._deployment.vault
    ));
    assert!(!Arc::ptr_eq(&first.maintenance, &second.maintenance));
    std::fs::write(owner.join("wrapping.key"), [84; 32]).unwrap();
    assert!(ManagerHost::from_root(root.path(), edge.clone()).is_err());
    assert_eq!(
        describe(&first).await.service_resolution.did,
        describe(&second).await.service_resolution.did
    );
    std::fs::write(owner.join("wrapping.key"), [83; 32]).unwrap();
    let restored = ManagerHost::from_root(root.path(), edge).unwrap();
    assert!(Arc::ptr_eq(
        &first._deployment.vault,
        &restored._deployment.vault
    ));
}

#[tokio::test]
async fn immediate_reload_reuses_a_vault_still_owned_after_host_wrapper_drop() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let edge = edge(home.path(), 69).await;
    provision(root.path(), &edge, 89);
    let first = ManagerHost::from_root(root.path(), edge.clone()).unwrap();
    let old_wrapper = Arc::downgrade(&first._deployment);
    let still_owned_vault = first._deployment.vault.clone();
    let prior_maintenance = first.maintenance.clone();
    prior_maintenance.stop();
    drop(first);
    assert!(old_wrapper.upgrade().is_none());
    // A real open redb is deliberately retained; no wait-for-close can mask
    // an attempt to open a second database handle during asynchronous cleanup.
    let reloaded = ManagerHost::from_root(root.path(), edge).unwrap();
    assert!(Arc::ptr_eq(&still_owned_vault, &reloaded._deployment.vault));
    assert!(!Arc::ptr_eq(&prior_maintenance, &reloaded.maintenance));
    describe(&reloaded).await;
}

#[tokio::test]
async fn cold_rebind_uses_durable_owner_and_rejects_a_different_service() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let original = edge(home.path(), 64).await;
    let other = edge(home.path(), 65).await;
    let directory = provision(root.path(), &original, 85);
    let host = ManagerHost::from_root(root.path(), original.clone()).unwrap();
    drop(host);
    wait_closed(&directory.join("archive.redb")).await;
    let vault = Arc::new(
        ManagerArchiveVault::open(
            &directory.join("archive.redb"),
            arkret::KeyBytes::new(vec![85; 32]),
        )
        .unwrap(),
    );
    assert!(ManagedActorManagerService::new(other, vault.clone()).is_err());
    let rebound = ManagedActorManagerService::new(original.clone(), vault.clone()).unwrap();
    drop(rebound);
    drop(vault);
    let reopened = ManagerHost::from_root(root.path(), original).unwrap();
    describe(&reopened).await;
}

#[tokio::test]
async fn missing_or_invalid_operator_key_does_not_create_a_manager_database() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let edge = edge(home.path(), 66).await;
    let directory = owner_directory(&root.path().canonicalize().unwrap(), &edge).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    assert!(ManagerHost::from_root(root.path(), edge.clone()).is_err());
    assert!(!directory.join("archive.redb").exists());
    std::fs::write(directory.join("wrapping.key"), [86; 31]).unwrap();
    assert!(ManagerHost::from_root(root.path(), edge).is_err());
    assert!(!directory.join("archive.redb").exists());
}

#[tokio::test]
async fn retirement_cancels_work_with_a_retained_host_arc_without_growing_task_registry() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let edge = edge(home.path(), 67).await;
    provision(root.path(), &edge, 87);
    let host = Arc::new(ManagerHost::from_root(root.path(), edge).unwrap());
    let lifetime = super::super::AppletLifetime::default();
    lifetime.own_manager(host.maintenance()).unwrap();
    let retained = host.clone();
    let work = lifetime.until_retired(async {
        describe(&retained).await;
        std::future::pending::<anyhow::Result<()>>().await
    });
    let retire = async {
        tokio::task::yield_now().await;
        lifetime.retire();
    };
    let (result, ()) = tokio::join!(work, retire);
    assert!(result.is_err());
    assert!(Arc::strong_count(&host) >= 2);
    assert!(lifetime.manager.lock().unwrap().is_none());
    assert!(lifetime.tasks.lock().unwrap().is_empty());
    assert!(lifetime.until_retired(async { Ok(()) }).await.is_err());
}

#[tokio::test]
async fn operator_archive_key_cannot_reuse_the_daily_or_service_key() {
    let home = tempfile::tempdir().unwrap();
    let edge = edge(home.path(), 68).await;
    for forbidden in [0x22, 68] {
        let root = tempfile::tempdir().unwrap();
        let directory = provision(root.path(), &edge, forbidden);
        assert!(ManagerHost::from_root(root.path(), edge.clone()).is_err());
        assert!(!directory.join("archive.redb").exists());
        std::fs::write(directory.join("wrapping.key"), [88; 32]).unwrap();
        let corrected = ManagerHost::from_root(root.path(), edge.clone()).unwrap();
        describe(&corrected).await;
    }
}

#[tokio::test]
async fn retirement_cancels_pending_body_before_host_initialization() {
    use salvo::test::ResponseExt as _;

    let home = tempfile::tempdir().unwrap();
    let config = savfox_channels::arkret::applet::ArkretAppletConfig::from_channel_config(
        &super::super::tests::valid_channel_config(),
    )
    .unwrap();
    let state = super::super::AppletChannelState {
        journal: super::super::build_applet_authoring_journal(home.path(), &config.id).unwrap(),
        config,
        edge: tokio::sync::Mutex::new(None),
        host: tokio::sync::OnceCell::new(),
        lifetime: super::super::AppletLifetime::default(),
    };
    let body_polled = std::sync::atomic::AtomicBool::new(false);
    let body = std::future::poll_fn(|_| {
        body_polled.store(true, std::sync::atomic::Ordering::Release);
        std::task::Poll::<Result<Vec<u8>, salvo::http::ParseError>>::Pending
    });
    let mut response = salvo::Response::new();
    let request = super::super::forward_applet_with_body(
        &state,
        body,
        salvo::http::Method::POST,
        "https://savfox.example/_arkret/edge/applet/transactions"
            .parse()
            .unwrap(),
        salvo::http::HeaderMap::new(),
        &mut response,
    );
    let retire = async {
        while !body_polled.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        state.lifetime.retire();
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(request, retire);
    })
    .await
    .expect("retirement must cancel the pending body read");
    assert_eq!(
        response.status_code,
        Some(salvo::http::StatusCode::SERVICE_UNAVAILABLE)
    );
    let problem: arkret::Problem = response.take_json().await.unwrap();
    assert_eq!(problem.code(), "temporarily_unavailable");
    assert!(state.host.get().is_none());
    assert!(state.edge.lock().await.is_none());
    assert!(state.lifetime.manager.lock().unwrap().is_none());
}

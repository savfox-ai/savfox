//! Operator-unlocked archive capabilities belong only to the management host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use arkret_bridge_runtime::managed_actor_authoring::{
    ManagedActorManagerService, ManagerArchiveMaintenance,
};
use arkret_bridge_runtime::manager_archive::ManagerArchiveVault;
use arkret_bridge_runtime::{ArkretEdge, BridgeError};

const ARCHIVE_ROOT_ENV: &str = "SAVFOX_ARKRET_MANAGER_ARCHIVE_ROOT";

struct Deployment {
    vault: Arc<ManagerArchiveVault>,
}

struct CachedVault {
    vault: Weak<ManagerArchiveVault>,
    key_digest: String,
}

fn deployments() -> &'static Mutex<BTreeMap<PathBuf, CachedVault>> {
    static DEPLOYMENTS: OnceLock<Mutex<BTreeMap<PathBuf, CachedVault>>> = OnceLock::new();
    DEPLOYMENTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn unavailable(detail: &str) -> BridgeError {
    BridgeError::Config(format!("Applet manager archive: {detail}"))
}

fn owner_directory(root: &Path, edge: &ArkretEdge) -> arkret_bridge_runtime::Result<PathBuf> {
    let applet = arkret::AppletId::new(edge.applet_id())?;
    let digest = arkret::canonical::canonical_sha256(&(applet, edge.service_actor_id()))?;
    let owner = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| unavailable("owner digest is not SHA-256"))?;
    Ok(root.join(owner))
}

fn open_deployment(
    root: &Path,
    edge: &ArkretEdge,
) -> arkret_bridge_runtime::Result<Arc<Deployment>> {
    let root = root
        .canonicalize()
        .map_err(|_| unavailable("operator root is unavailable"))?;
    if !root.is_dir() {
        return Err(unavailable("operator root is not a directory"));
    }
    let owner = owner_directory(&root, edge)?;
    let directory = owner
        .canonicalize()
        .map_err(|_| unavailable("owner directory is unavailable"))?;
    if directory != owner {
        return Err(unavailable(
            "owner directory must not alias another deployment",
        ));
    }
    let key = arkret::KeyBytes::new(
        std::fs::read(directory.join("wrapping.key"))
            .map_err(|_| unavailable("operator-provided wrapping key is unavailable"))?,
    );
    if key.as_slice().len() != 32 {
        return Err(unavailable(
            "operator-provided wrapping key must contain exactly 32 bytes",
        ));
    }
    // Validate role separation before opening redb: a mistaken operator key
    // must not leave behind a newly sealed store that blocks corrected setup.
    ManagedActorManagerService::validate_archive_wrapping_key(edge, &key)?;
    let database = directory.join("archive.redb");
    if database.exists()
        && database
            .canonicalize()
            .map_err(|_| unavailable("archive database is unavailable"))?
            != database
    {
        return Err(unavailable("archive database must not alias another store"));
    }
    let key_digest = arkret::canonical::sha256_digest(key.as_slice());
    let mut known = deployments()
        .lock()
        .map_err(|_| unavailable("deployment registry is unavailable"))?;
    known.retain(|_, cached| cached.vault.strong_count() != 0);
    if let Some((vault, prior_key_digest)) = known.get(&database).and_then(|cached| {
        cached
            .vault
            .upgrade()
            .map(|vault| (vault, &cached.key_digest))
    }) {
        if prior_key_digest != &key_digest {
            return Err(unavailable(
                "operator wrapping key changed while the archive is open",
            ));
        }
        return Ok(Arc::new(Deployment { vault }));
    }
    let vault = Arc::new(ManagerArchiveVault::open(&database, key)?);
    known.insert(
        database,
        CachedVault {
            vault: Arc::downgrade(&vault),
            key_digest,
        },
    );
    Ok(Arc::new(Deployment { vault }))
}

pub(super) struct ManagerHost {
    host: arkret_bridge_runtime::web::ManagedAppletHttpHost,
    maintenance: Arc<ManagerArchiveMaintenance>,
    _deployment: Arc<Deployment>,
}

impl ManagerHost {
    pub(super) fn from_environment(edge: Arc<ArkretEdge>) -> arkret_bridge_runtime::Result<Self> {
        let root = std::env::var_os(ARCHIVE_ROOT_ENV)
            .ok_or_else(|| unavailable("SAVFOX_ARKRET_MANAGER_ARCHIVE_ROOT is required"))?;
        Self::from_root(&PathBuf::from(root), edge)
    }

    pub(super) fn from_root(
        root: &Path,
        edge: Arc<ArkretEdge>,
    ) -> arkret_bridge_runtime::Result<Self> {
        let deployment = open_deployment(root, &edge)?;
        let manager = Arc::new(ManagedActorManagerService::new(
            edge.clone(),
            deployment.vault.clone(),
        )?);
        let host =
            arkret_bridge_runtime::web::ManagedAppletHttpHost::with_manager(edge, manager.clone())?;
        // The router already started this exact maintenance owner. Retain its
        // existing guard so local retirement can stop it despite in-flight Arcs.
        let maintenance = manager.start_maintenance()?;
        Ok(Self {
            host,
            maintenance,
            _deployment: deployment,
        })
    }

    pub(super) fn maintenance(&self) -> Arc<ManagerArchiveMaintenance> {
        self.maintenance.clone()
    }

    pub(super) async fn receive(
        &self,
        method: salvo::http::Method,
        uri: salvo::http::uri::Uri,
        headers: salvo::http::HeaderMap,
        body: Vec<u8>,
    ) -> anyhow::Result<(salvo::http::StatusCode, salvo::http::HeaderMap, Vec<u8>)> {
        self.host
            .receive(method, uri, headers, body)
            .await
            .map_err(anyhow::Error::from)
    }
}

#[cfg(test)]
mod tests;

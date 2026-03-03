use anyhow::Context;
use anyhow::Result;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::Rng;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CapSids {
    pub workspace: String,
    pub readonly: String,
}

pub fn cap_sid_file(savfox_home: &Path) -> PathBuf {
    savfox_home.join("cap_sid")
}

fn make_random_cap_sid_string() -> String {
    let mut os_rng = rand::rng();
    let mut rng = SmallRng::from_rng(&mut os_rng);
    let a = rng.next_u32();
    let b = rng.next_u32();
    let c = rng.next_u32();
    let d = rng.next_u32();
    format!("S-1-5-21-{}-{}-{}-{}", a, b, c, d)
}

fn persist_caps(path: &Path, caps: &CapSids) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .with_context(|| format!("create cap sid dir {}", dir.display()))?;
    }
    let json = serde_json::to_string(caps)?;
    fs::write(path, json).with_context(|| format!("write cap sid file {}", path.display()))?;
    Ok(())
}

pub fn load_or_create_cap_sids(savfox_home: &Path) -> Result<CapSids> {
    let path = cap_sid_file(savfox_home);
    if path.exists() {
        let txt = fs::read_to_string(&path)
            .with_context(|| format!("read cap sid file {}", path.display()))?;
        let t = txt.trim();
        if t.starts_with('{') && t.ends_with('}') {
            if let Ok(obj) = serde_json::from_str::<CapSids>(t) {
                return Ok(obj);
            }
        } else if !t.is_empty() {
            let caps = CapSids {
                workspace: t.to_string(),
                readonly: make_random_cap_sid_string(),
            };
            persist_caps(&path, &caps)?;
            return Ok(caps);
        }
    }
    let caps = CapSids {
        workspace: make_random_cap_sid_string(),
        readonly: make_random_cap_sid_string(),
    };
    persist_caps(&path, &caps)?;
    Ok(caps)
}

//! Random names for new terminals, loaded from JSON name-packs.
//!
//! Bundled packs live in repo-root `name-packs/` and ship into the install
//! directory via `tauri.conf.json` `bundle.resources`. Extra packs can be
//! dropped next to the exe (`<install>/name-packs/`) or under
//! `<data_root>/name-packs/`. The active pack id is the `name_pack` setting
//! (default `tica-cats`). A compiled-in copy of that pack is the fallback
//! when no JSON is on disk (tests / broken installs).
//!
//! A random name is picked per new terminal and marked used; once every name
//! has been used the pool resets, so duplicates only appear after a full cycle.

use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Settings KV key: which name-pack id to draw new terminal titles from.
pub const NAME_PACK_KEY: &str = "name_pack";

/// Built-in pack id — matches `name-packs/tica-cats.json`.
pub const DEFAULT_PACK_ID: &str = "tica-cats";

/// TICA-recognised breeds (Chinese display names) plus a few nicknames.
/// Kept as the compiled-in fallback so naming still works if JSON is missing.
const FALLBACK_NAMES: &[&str] = &[
    "阿比西尼亚",
    "美国短毛",
    "美国卷耳",
    "美国短尾",
    "巴厘",
    "孟加拉豹猫",
    "伯曼",
    "孟买",
    "英国短毛",
    "缅甸",
    "伯米拉",
    "沙特尔",
    "乔西",
    "康沃耳雷克斯",
    "德文雷克斯",
    "埃及猫",
    "欧洲短毛",
    "异国短毛",
    "哈瓦那棕",
    "日本短尾",
    "考拉",
    "克拉特",
    "拉波尔",
    "缅因猫",
    "曼岛猫",
    "曼基康",
    "尼伯龙",
    "挪威森林",
    "奥西",
    "东方短毛",
    "波斯",
    "彼得秃",
    "皮克斯",
    "褴褛",
    "布偶",
    "俄罗斯蓝",
    "热带草原",
    "苏格兰折耳",
    "塞尔柯克雷克斯",
    "暹罗",
    "西伯利亚",
    "新加坡",
    "雪鞋",
    "索科科",
    "索马里",
    "斯芬克斯",
    "泰国",
    "东奇尼",
    "托伊格",
    "土耳其安哥拉",
    "土耳其梵",
    "比比拉布",
    "刀盾",
    "哈基米",
    "曼波",
    "南北绿豆",
    "巴巴博一",
    "歪比巴布",
    "欧润吉",
];

const MAX_PACK_BYTES: u64 = 256 * 1024;
const MAX_NAME_CHARS: usize = 64;
const MAX_NAMES: usize = 2000;

#[derive(Debug, Clone)]
pub struct NamePackInfo {
    pub id: String,
    pub name: String,
    pub note: String,
    pub count: usize,
}

#[derive(Debug, Clone)]
struct Pack {
    id: String,
    name: String,
    note: String,
    names: Vec<String>,
}

#[derive(Deserialize)]
struct PackObject {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    note: Option<String>,
    names: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PackDoc {
    Object(PackObject),
    List(Vec<String>),
}

struct Registry {
    packs: Vec<Pack>,
    active_id: String,
    used: HashSet<usize>,
    /// Last resource dir we scanned, so a second init can refresh.
    resource_dir: Option<PathBuf>,
}

impl Registry {
    fn with_fallback() -> Self {
        Self {
            packs: vec![fallback_pack()],
            active_id: DEFAULT_PACK_ID.to_string(),
            used: HashSet::new(),
            resource_dir: None,
        }
    }

    fn active(&self) -> &Pack {
        self.packs
            .iter()
            .find(|p| p.id == self.active_id)
            .or_else(|| self.packs.first())
            .expect("registry always has at least the fallback pack")
    }
}

static REGISTRY: LazyLock<Mutex<Registry>> =
    LazyLock::new(|| Mutex::new(Registry::with_fallback()));

fn lock_registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|p| p.into_inner())
}

fn fallback_pack() -> Pack {
    Pack {
        id: DEFAULT_PACK_ID.to_string(),
        name: "TICA 猫种".to_string(),
        note: "认证品种与几个外号".to_string(),
        names: FALLBACK_NAMES.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn sanitize_pack_id(raw: &str) -> Option<String> {
    let id = raw.trim();
    if id.is_empty() || id.len() > 64 {
        return None;
    }
    let mut chars = id.chars();
    let first = chars.next()?;
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return None;
    }
    Some(id.to_string())
}

fn clean_names(raw: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for name in raw {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            continue;
        }
        if seen.insert(name.to_string()) {
            out.push(name.to_string());
        }
        if out.len() >= MAX_NAMES {
            break;
        }
    }
    out
}

fn parse_pack(stem: &str, bytes: &[u8]) -> Result<Pack, String> {
    if bytes.len() as u64 > MAX_PACK_BYTES {
        return Err(format!("名称库超过 {MAX_PACK_BYTES} 字节上限"));
    }
    let doc: PackDoc = serde_json::from_slice(bytes).map_err(|e| {
        format!("JSON 无法解析：{e}。需要对象 {{\"id\",\"name\",\"note\",\"names\":[…]}} 或字符串数组 [\"甲\",\"乙\"]")
    })?;
    let (id_hint, name, note, names) = match doc {
        PackDoc::Object(o) => (o.id, o.name, o.note, o.names),
        PackDoc::List(names) => (None, None, None, names),
    };
    let names = clean_names(names);
    if names.is_empty() {
        return Err("names 为空（每项 1–64 字，空白项会被丢掉）".into());
    }
    let id = id_hint
        .as_deref()
        .and_then(sanitize_pack_id)
        .or_else(|| sanitize_pack_id(stem))
        .ok_or_else(|| {
            String::from(
                "缺少合法 id：用小写字母/数字开头，只含 a-z 0-9 - _，最长 64；对象可写 \"id\"，否则用文件名",
            )
        })?;
    Ok(Pack {
        name: name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.clone()),
        note: note.unwrap_or_default().trim().to_string(),
        id,
        names,
    })
}

fn load_packs_from_dir(dir: &Path, into: &mut Vec<Pack>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() > MAX_PACK_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("pack");
        if let Ok(pack) = parse_pack(stem, &bytes) {
            if let Some(existing) = into.iter_mut().find(|p| p.id == pack.id) {
                *existing = pack;
            } else {
                into.push(pack);
            }
        }
    }
}

fn discover_pack_dirs(resource_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(r) = resource_dir {
        dirs.push(r.join("name-packs"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("name-packs"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd;
        for _ in 0..6 {
            let candidate = dir.join("name-packs");
            if candidate.is_dir() {
                dirs.push(candidate);
                break;
            }
            if !dir.pop() {
                break;
            }
        }
    }
    dirs.push(crate::persistence::data_root().join("name-packs"));
    dirs
}

/// Scan disk for JSON packs. Later directories override the same id, so a
/// user copy under `<data_root>/name-packs/` wins over the bundled file.
/// Always keeps the compiled-in fallback if `tica-cats` is absent on disk.
pub fn refresh_from_disk(resource_dir: Option<&Path>) {
    let mut packs = Vec::new();
    for dir in discover_pack_dirs(resource_dir) {
        load_packs_from_dir(&dir, &mut packs);
    }
    if !packs.iter().any(|p| p.id == DEFAULT_PACK_ID) {
        packs.insert(0, fallback_pack());
    }
    packs.sort_by(|a, b| a.id.cmp(&b.id));

    let mut reg = lock_registry();
    let prev_active = reg.active_id.clone();
    reg.packs = packs;
    reg.resource_dir = resource_dir.map(|p| p.to_path_buf());
    if !reg.packs.iter().any(|p| p.id == prev_active) {
        reg.active_id = DEFAULT_PACK_ID.to_string();
        reg.used.clear();
    }
}

/// Apply a persisted pack id. Unknown ids fall back to [`DEFAULT_PACK_ID`].
pub fn set_active_id(id: &str) -> String {
    let mut reg = lock_registry();
    let next = if reg.packs.iter().any(|p| p.id == id) {
        id.to_string()
    } else {
        DEFAULT_PACK_ID.to_string()
    };
    if next != reg.active_id {
        reg.active_id = next.clone();
        reg.used.clear();
    }
    next
}

pub fn active_id() -> String {
    lock_registry().active_id.clone()
}

pub fn list_packs() -> Vec<NamePackInfo> {
    lock_registry()
        .packs
        .iter()
        .map(|p| NamePackInfo {
            id: p.id.clone(),
            name: p.name.clone(),
            note: p.note.clone(),
            count: p.names.len(),
        })
        .collect()
}

/// User-writable pack directory (`<data_root>/name-packs/`). Imports land here
/// so they survive app upgrades and don't need a writable install root.
pub fn user_pack_dir() -> PathBuf {
    crate::persistence::data_root().join("name-packs")
}

/// Validate JSON, write it under [`user_pack_dir`], refresh the in-memory
/// registry, and return the installed pack id.
///
/// `stem_hint` is used when the JSON is a bare array (or the object has no
/// `id`): file basename without `.json`, or `"pasted"` for clipboard imports.
pub fn install_user_pack(bytes: &[u8], stem_hint: &str) -> Result<String, String> {
    let stem = sanitize_pack_id(stem_hint).unwrap_or_else(|| "pasted".to_string());
    let pack = parse_pack(&stem, bytes)?;
    let dir = user_pack_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建 {}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.json", pack.id));
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            return Err("拒绝通过符号链接覆盖名称库".into());
        }
    }
    std::fs::write(&path, bytes).map_err(|e| format!("写入失败 {}: {e}", path.display()))?;
    let id = pack.id;
    let resource = lock_registry().resource_dir.clone();
    refresh_from_disk(resource.as_deref());
    Ok(id)
}

/// Pick a random name for a new terminal. Names already attached to persisted
/// sessions are excluded, so restarting the IDE cannot create a duplicate
/// among visible terminals. The in-process set also closes the race between
/// concurrent spawns whose persistence snapshots were taken together.
pub fn next_breed_excluding(existing_titles: &HashSet<String>) -> String {
    let mut reg = lock_registry();
    // Clone names so we can mutate `used` without fighting the pack borrow.
    let names = reg.active().names.clone();
    let n = names.len();
    if n == 0 {
        return "终端".to_string();
    }
    if reg.used.len() >= n {
        reg.used.clear();
    }

    let available: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(i, name)| !reg.used.contains(i) && !existing_titles.contains(*name))
        .map(|(i, _)| i)
        .collect();

    let candidates = if available.is_empty() {
        reg.used.clear();
        names
            .iter()
            .enumerate()
            .filter(|(_, name)| !existing_titles.contains(*name))
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    } else {
        available
    };
    let candidates = if candidates.is_empty() {
        (0..n).collect::<Vec<_>>()
    } else {
        candidates
    };
    let pick = (uuid::Uuid::new_v4().as_u128() % candidates.len() as u128) as usize;
    let index = candidates[pick];
    reg.used.insert(index);
    names[index].clone()
}

/// Pick without external exclusions (primarily useful in isolated tests).
#[cfg(test)]
pub fn next_breed() -> String {
    next_breed_excluding(&HashSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All tests mutate the process-global registry.
    static PACK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_fallback() {
        let mut reg = lock_registry();
        reg.packs = vec![fallback_pack()];
        reg.active_id = DEFAULT_PACK_ID.to_string();
        reg.used.clear();
        reg.resource_dir = None;
    }

    #[test]
    fn fallback_names_are_unique_and_pool_avoids_repeats() {
        let _guard = PACK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_fallback();
        let mut seen = HashSet::new();
        for b in FALLBACK_NAMES {
            assert!(seen.insert(*b), "duplicate breed in list: {b}");
        }
        let mut picks = HashSet::new();
        for _ in 0..FALLBACK_NAMES.len() {
            picks.insert(next_breed());
        }
        assert_eq!(picks.len(), FALLBACK_NAMES.len());

        lock_registry().used.clear();
        let existing: HashSet<String> = ["布偶".to_string(), "奥西".to_string()]
            .into_iter()
            .collect();
        for _ in 0..FALLBACK_NAMES.len() - existing.len() {
            let picked = next_breed_excluding(&existing);
            assert!(!existing.contains(&picked));
        }
    }

    #[test]
    fn parse_object_and_bare_array() {
        let _guard = PACK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let obj = r#"{"id":"demo","name":"Demo","note":"n","names":["甲","乙","甲"]}"#;
        let pack = parse_pack("ignored", obj.as_bytes()).expect("object pack");
        assert_eq!(pack.id, "demo");
        assert_eq!(pack.names, vec!["甲", "乙"]);

        let list = r#"["一","二"]"#;
        let pack = parse_pack("my-pack", list.as_bytes()).expect("list pack");
        assert_eq!(pack.id, "my-pack");
        assert_eq!(pack.names, vec!["一", "二"]);
    }

    #[test]
    fn disk_pack_overrides_fallback_and_switch_resets_cycle() {
        let _guard = PACK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_fallback();
        let dir = std::env::temp_dir().join(format!(
            "capilot-name-packs-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("stars.json"),
            r#"{"id":"stars","name":"Stars","names":["织女","牛郎","天狼"]}"#,
        )
        .unwrap();
        load_packs_from_dir(&dir, &mut lock_registry().packs);
        assert!(lock_registry().packs.iter().any(|p| p.id == "stars"));

        let applied = set_active_id("stars");
        assert_eq!(applied, "stars");
        let mut picks = HashSet::new();
        for _ in 0..3 {
            picks.insert(next_breed());
        }
        assert_eq!(picks.len(), 3);
        assert!(picks.contains("织女"));

        set_active_id("tica-cats");
        assert_eq!(active_id(), "tica-cats");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! TICA-certified cat breed names used to randomly name new terminals.
//!
//! Kept in one dedicated file so the list is easy to review / extend. A random
//! breed is picked per new terminal and marked used; once every breed has been
//! used the pool resets, so duplicates only appear after a full cycle.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// TICA-recognised breeds (Chinese display names), e.g. 布偶 / 奥西 / 热带草原.
pub const BREEDS: &[&str] = &[
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
];

static USED: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Pick a random TICA breed name for a new terminal. Names already attached to
/// persisted sessions are excluded, so restarting the IDE cannot create a
/// duplicate among visible terminals. The in-process set also closes the race
/// between concurrent spawns whose persistence snapshots were taken together.
pub fn next_breed_excluding(existing_titles: &HashSet<String>) -> &'static str {
    let mut used = USED.lock().unwrap();
    if used.len() >= BREEDS.len() {
        used.clear();
    }

    let available: Vec<usize> = BREEDS
        .iter()
        .enumerate()
        .filter(|(i, name)| !used.contains(i) && !existing_titles.contains(**name))
        .map(|(i, _)| i)
        .collect();

    // More than BREEDS.len() live sessions necessarily requires reuse. Prefer
    // names absent from the current process cycle before falling back to the
    // complete pool.
    let candidates = if available.is_empty() {
        used.clear();
        BREEDS
            .iter()
            .enumerate()
            .filter(|(_, name)| !existing_titles.contains(**name))
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    } else {
        available
    };
    let candidates = if candidates.is_empty() {
        (0..BREEDS.len()).collect::<Vec<_>>()
    } else {
        candidates
    };
    let pick = (uuid::Uuid::new_v4().as_u128() % candidates.len() as u128) as usize;
    let index = candidates[pick];
    used.insert(index);
    BREEDS[index]
}

/// Pick without external exclusions (primarily useful in isolated tests).
#[cfg(test)]
pub fn next_breed() -> &'static str {
    next_breed_excluding(&HashSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breeds_are_unique_and_pool_avoids_repeats() {
        let mut seen = std::collections::HashSet::new();
        for b in BREEDS {
            assert!(seen.insert(*b), "duplicate breed in list: {b}");
        }
        // First pass over the pool returns every breed exactly once.
        let mut picks = std::collections::HashSet::new();
        for _ in 0..BREEDS.len() {
            picks.insert(next_breed());
        }
        assert_eq!(picks.len(), BREEDS.len());

        // Persisted names remain excluded even after the in-memory cycle state
        // is cleared, which models an IDE restart.
        USED.lock().unwrap().clear();
        let existing: HashSet<String> = ["布偶".to_string(), "奥西".to_string()]
            .into_iter()
            .collect();
        for _ in 0..BREEDS.len() - existing.len() {
            let picked = next_breed_excluding(&existing);
            assert!(!existing.contains(picked));
        }
    }
}

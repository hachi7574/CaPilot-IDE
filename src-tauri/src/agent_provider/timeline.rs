//! Canonical timeline store (architecture §6.2, §10.2).
//!
//! The manager keeps one [`TimelineStore`] per agent. It upserts timeline items
//! by stable `item_id` (streaming updates reuse the same id; the UI never
//! dedups by text). The store is in-memory for Phase 1; the daemon persists a
//! JSON snapshot and replays it on reconnect.

use crate::agent_provider::types::{ItemStatus, TimelineEvent, TimelineItem};
use std::collections::HashMap;

/// Ordered timeline for one agent.
#[derive(Debug, Clone, Default)]
pub struct TimelineStore {
    /// item_id → current item (upserted by streaming events).
    items: HashMap<String, TimelineItem>,
    /// Stable append order of item ids (replacement preserves position).
    order: Vec<String>,
}

impl TimelineStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one timeline mutation.
    pub fn apply(&mut self, event: &TimelineEvent) {
        match event {
            TimelineEvent::Started { item } | TimelineEvent::Replaced { item } => {
                let id = item.item_id().to_string();
                let is_new = !self.items.contains_key(&id);
                if is_new {
                    self.order.push(id.clone());
                }
                self.items.insert(id, item.clone());
            }
            TimelineEvent::Appended {
                item_id,
                text_delta,
            } => {
                if let Some(
                    TimelineItem::UserMessage(m)
                    | TimelineItem::AssistantMessage(m)
                    | TimelineItem::Reasoning(m),
                ) = self.items.get_mut(item_id)
                {
                    m.text.push_str(text_delta);
                }
            }
            TimelineEvent::Finished { item_id, status } => {
                if let Some(TimelineItem::ToolCall(tool)) = self.items.get_mut(item_id) {
                    tool.status = match status {
                        ItemStatus::Pending => tool.status.clone(),
                        ItemStatus::Complete => {
                            crate::agent_provider::types::ToolCallStatus::Completed
                        }
                        ItemStatus::Failed => crate::agent_provider::types::ToolCallStatus::Failed,
                        ItemStatus::Cancelled => {
                            crate::agent_provider::types::ToolCallStatus::Cancelled
                        }
                    };
                }
            }
        }
    }

    /// Snapshot of the timeline in append order.
    pub fn items(&self) -> Vec<TimelineItem> {
        self.order
            .iter()
            .filter_map(|id| self.items.get(id).cloned())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether an item id already exists (streaming updates append to it).
    pub fn contains(&self, item_id: &str) -> bool {
        self.items.contains_key(item_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_provider::types::{MessageItem, MessageRole, ToolCallItem};

    fn assistant_started(id: &str, text: &str) -> TimelineEvent {
        TimelineEvent::Started {
            item: TimelineItem::AssistantMessage(MessageItem {
                item_id: id.into(),
                role: MessageRole::Assistant,
                text: text.into(),
                created_at: 1,
                metadata: None,
            }),
        }
    }

    #[test]
    fn started_then_appended_builds_one_item() {
        let mut store = TimelineStore::new();
        store.apply(&assistant_started("a1", "hello"));
        store.apply(&TimelineEvent::Appended {
            item_id: "a1".into(),
            text_delta: " world".into(),
        });
        let items = store.items();
        assert_eq!(items.len(), 1);
        match &items[0] {
            TimelineItem::AssistantMessage(m) => assert_eq!(m.text, "hello world"),
            other => panic!("unexpected item {other:?}"),
        }
    }

    #[test]
    fn replaced_preserves_position() {
        let mut store = TimelineStore::new();
        store.apply(&assistant_started("a1", "one"));
        store.apply(&assistant_started("a2", "two"));
        store.apply(&TimelineEvent::Replaced {
            item: TimelineItem::AssistantMessage(MessageItem {
                item_id: "a1".into(),
                role: MessageRole::Assistant,
                text: "one-updated".into(),
                created_at: 2,
                metadata: None,
            }),
        });
        let items = store.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].item_id(), "a1");
        assert_eq!(items[1].item_id(), "a2");
        match &items[0] {
            TimelineItem::AssistantMessage(m) => assert_eq!(m.text, "one-updated"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn finish_sets_tool_status() {
        let mut store = TimelineStore::new();
        store.apply(&TimelineEvent::Started {
            item: TimelineItem::ToolCall(ToolCallItem {
                item_id: "t1".into(),
                tool_name: "read".into(),
                tool_input: None,
                tool_output: None,
                status: crate::agent_provider::types::ToolCallStatus::Running,
                created_at: 1,
                metadata: None,
            }),
        });
        store.apply(&TimelineEvent::Finished {
            item_id: "t1".into(),
            status: ItemStatus::Complete,
        });
        match &store.items()[0] {
            TimelineItem::ToolCall(t) => assert_eq!(
                t.status,
                crate::agent_provider::types::ToolCallStatus::Completed
            ),
            other => panic!("unexpected {other:?}"),
        }
    }
}

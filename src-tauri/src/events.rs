// 主进程 → 渲染进程的事件负载（对齐 preload 的广播通道）

use serde::Serialize;

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct DataUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reinit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healed: Option<bool>,
}

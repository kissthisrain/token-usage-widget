// 工具自动探测：扫描已知路径，识别 ZCode / Codex / DSH / WorkBuddy / OpenCode CLI（对齐 core/detect.js）

use crate::config::SourceConfig;
use crate::sources::local_logs::{list_jsonl_files, parse_file, scan_params_for_source};
use crate::util::{date_key, now_ms};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    pub id: String,
    pub name: String,
    pub tool: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub live: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct Profile {
    id: &'static str,
    name: &'static str,
    tool: &'static str,
    paths: Vec<PathBuf>,
    recursive: bool,
    live: bool,
    desc: &'static str,
}

const SCAN_FILE_LIMIT: usize = 800;

fn profiles() -> Vec<Profile> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    vec![
        Profile {
            id: "zcode",
            name: "ZCode",
            tool: "zcode",
            paths: vec![home.join(".zcode").join("cli").join("rollout")],
            recursive: false,
            live: true,
            desc: "ZCode 客户端会话记录（实时写入）",
        },
        Profile {
            id: "codex",
            name: "Codex",
            tool: "codex",
            paths: vec![home.join(".codex").join("sessions")],
            recursive: true,
            live: false,
            desc: "Codex CLI 会话记录（含历史用量与周配额）",
        },
        Profile {
            id: "dsh",
            name: "DSH",
            tool: "dsh",
            paths: vec![home.join(".dsh").join("sessions")],
            recursive: true,
            live: true,
            desc: "DeepSeek Harness 会话记录（实时写入）",
        },
        Profile {
            id: "workbuddy-intl",
            name: "workbuddy-intl",
            // 两版日志格式完全一致 → 共用同一个 tool（解析器），仅 id/name/路径不同
            tool: "workbuddy",
            paths: vec![home.join(".workbuddy-ai").join("projects")],
            recursive: true,
            live: true,
            desc: "WorkBuddy 国际版会话记录（~/.workbuddy-ai/projects · workbuddy.ai，实时写入）",
        },
        Profile {
            id: "workbuddy-cn",
            name: "workbuddy-cn",
            tool: "workbuddy",
            paths: vec![home.join(".workbuddy").join("projects")],
            recursive: true,
            live: true,
            desc: "WorkBuddy 国内版会话记录（~/.workbuddy/projects · workbuddy.cn，实时写入）",
        },
        Profile {
            id: "opencode",
            name: "OpenCode CLI",
            tool: "zcode",
            paths: vec![
                home.join(".local").join("share").join("opencode"),
                home.join(".config").join("opencode"),
            ],
            recursive: true,
            live: true,
            desc: "OpenCode CLI 会话记录（如已安装）",
        },
    ]
}

fn scan_profile(p: &Profile) -> Detection {
    let dir = p.paths.iter().find(|d| d.is_dir()).cloned();
    let Some(dir) = dir else {
        return Detection {
            id: p.id.to_string(),
            name: p.name.to_string(),
            tool: p.tool.to_string(),
            exists: false,
            live: p.live,
            ..Default::default()
        };
    };
    let params = scan_params_for_source(&SourceConfig {
        tool: Some(p.tool.to_string()),
        ..Default::default()
    });
    let mut files = list_jsonl_files(&dir, p.recursive, &params.pattern);
    if files.len() > SCAN_FILE_LIMIT {
        files = files[files.len() - SCAN_FILE_LIMIT..].to_vec();
    }

    let mut record_count: usize = 0;
    let mut date_from: Option<String> = None;
    let mut date_to: Option<String> = None;
    let mut models: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for f in &files {
        let (records, rate_limits) = parse_file(f, &params.format);
        for r in &records {
            record_count += 1;
            let ts = r.ts.unwrap_or_else(now_ms);
            let k = date_key(ts);
            if date_from.as_deref().is_none_or(|d| k.as_str() < d) {
                date_from = Some(k.clone());
            }
            if date_to.as_deref().is_none_or(|d| k.as_str() > d) {
                date_to = Some(k);
            }
            if let Some(m) = &r.model {
                models.insert(m.clone());
            }
        }
        if !rate_limits.is_empty() {
            models.insert("rate_limits".to_string());
        }
        if files.len() > 60 && record_count > 50000 {
            break;
        }
    }

    Detection {
        id: p.id.to_string(),
        name: p.name.to_string(),
        tool: p.tool.to_string(),
        exists: true,
        path: Some(dir.to_string_lossy().to_string()),
        file_count: Some(files.len()),
        record_count: Some(record_count),
        date_from,
        date_to,
        models: Some(models.into_iter().collect()),
        live: p.live,
        desc: Some(p.desc.to_string()),
        ..Default::default()
    }
}

pub fn scan_all() -> Vec<Detection> {
    profiles()
        .iter()
        .map(|p| {
            match std::panic::catch_unwind(|| scan_profile(p)) {
                Ok(d) => d,
                Err(_) => Detection {
                    id: p.id.to_string(),
                    name: p.name.to_string(),
                    tool: p.tool.to_string(),
                    exists: false,
                    error: Some("扫描异常".to_string()),
                    ..Default::default()
                },
            }
        })
        .collect()
}

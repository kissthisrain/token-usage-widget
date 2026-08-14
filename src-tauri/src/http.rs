// 轻量 HTTP 客户端（对齐 core/http.js）：
// - 默认 UA token-usage-widget/0.1
// - 超时、最多 3 次重定向跟随
// - content-type 含 json 时自动解析 body；否则保留原文

use once_cell::sync::Lazy;
use std::time::Duration;

pub struct HttpRes {
    pub status: u16,
    #[allow(dead_code)] // 二期 probeQuota 需要读取响应头（location）
    pub headers: Vec<(String, String)>,
    pub body_json: Option<serde_json::Value>,
    pub body_text: String,
    /// 手动跟随过的重定向 URL 链（供调用方识别登录跳转等）
    pub redirects: Vec<String>,
}

impl HttpRes {
    #[allow(dead_code)]
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }
}

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("token-usage-widget/0.1")
        .redirect(reqwest::redirect::Policy::none()) // 手动跟随（对齐 JS 最多 3 次）
        .build()
        .expect("http client build")
});

/// GET + JSON 自动解析；重定向最多 3 次（仅限同主机、不降级 scheme，防止凭据跨域转发）；
/// 失败返回 Err(消息)
pub async fn get_json(
    url: &str,
    headers: &[(String, String)],
    timeout_ms: u64,
) -> Result<HttpRes, String> {
    // 记录起始 host/scheme：跨主机或 https→http 降级时不再跟随重定向，
    // 避免 Authorization / Cookie 等凭据被转发到第三方域名
    let original = reqwest::Url::parse(url).ok();
    let orig_host = original
        .as_ref()
        .and_then(|u| u.host_str())
        .map(|s| s.to_lowercase());
    let orig_https = original.as_ref().map(|u| u.scheme() == "https").unwrap_or(false);
    let mut redirects: Vec<String> = Vec::new();
    let mut current = url.to_string();
    for _ in 0..4 {
        let mut req = CLIENT
            .get(&current)
            .timeout(Duration::from_millis(timeout_ms));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| format!("请求失败: {e}"))?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let bytes = resp.bytes().await.map_err(|e| format!("读取响应失败: {e}"))?;
        // 3xx + location → 跟随（仅同主机 + 非降级）
        if (300..400).contains(&status) {
            if let Some(loc) = headers
                .iter()
                .find(|(k, _)| k.to_lowercase() == "location")
                .map(|(_, v)| v.clone())
            {
                let next = reqwest::Url::parse(&current)
                    .ok()
                    .and_then(|base| base.join(&loc).ok());
                if let Some(u) = next {
                    let same_host = match (&orig_host, u.host_str()) {
                        (Some(h), Some(nh)) => nh.eq_ignore_ascii_case(h),
                        _ => false,
                    };
                    let scheme_ok = !orig_https || u.scheme() == "https";
                    if same_host && scheme_ok {
                        redirects.push(u.to_string());
                        current = u.to_string();
                        continue;
                    }
                }
                // 跨主机 / 降级：不跟随，把当前 3xx 响应返回给调用方
            }
        }
        let ct: String = headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let body_json = if ct.to_lowercase().contains("json") {
            serde_json::from_str(&text).ok()
        } else {
            None
        };
        return Ok(HttpRes {
            status,
            headers,
            body_json,
            body_text: text,
            redirects,
        });
    }
    Err("重定向次数过多".to_string())
}

// 本地 HTTP 代理（二期）：客户端把 API Base URL 指向 http://127.0.0.1:<port>
// 转发到 opencode.ai/zen/go（可配置 baseUrl），透传 Authorization 头
// - SSE 流式（accept 含 text/event-stream）：边转发边逐行解析 data: 提取 usage
// - 非流式：收集完整响应后解析 usage
// - 兼容 OpenAI chat.completions / responses / Anthropic messages 三种字段风格
// - 结合价目表计算美元费用 → 写历史 → 广播 data:update

use crate::config::SourceConfig;
use crate::events::DataUpdate;
use crate::history::HistoryStore;
use crate::sources::opencode_go::{cost_for, source_price_map};
use bytes::Bytes;
use futures_util::stream::Stream;
use futures_util::StreamExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Incoming;
use hyper::header::HeaderValue;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Emitter;

/// 请求体上限（防本地进程灌入超大请求）
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
/// 上游请求总超时（含流式读取；SSE 长连接有数据即续期）
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);
/// 本应用 webview 的受信 Origin（CSP connect-src 之外的浏览器来源一律拒绝）
const TRUSTED_ORIGINS: [&str; 3] = ["tauri://localhost", "http://ipc.localhost", "https://tauri.localhost"];

type ProxyBody = UnsyncBoxBody<Bytes, io::Error>;

fn full_bytes(b: Bytes) -> ProxyBody {
    UnsyncBoxBody::new(Full::new(b).map_err(io::Error::other))
}

fn stream_body<S>(s: S) -> ProxyBody
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    UnsyncBoxBody::new(StreamBody::new(s.map(|r| r.map(hyper::body::Frame::data))))
}

fn json_response(status: StatusCode, body: &str) -> Response<ProxyBody> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full_bytes(Bytes::from(body.to_string())))
        .unwrap_or_else(|_| Response::new(full_bytes(Bytes::new())))
}

/// 认证判定（独立函数便于单测）：token 必须匹配，且带 Origin 时必须是受信来源
fn auth_ok(token: &str, parts: &hyper::http::request::Parts) -> bool {
    if let Some(origin) = parts.headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !origin.is_empty() && !TRUSTED_ORIGINS.contains(&origin) {
            return false;
        }
    }
    let tok = parts
        .headers
        .get("x-proxy-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    tok == token
}

pub struct GoProxy {
    inner: Arc<GoProxyInner>,
}

struct GoProxyInner {
    get_source: Arc<dyn Fn() -> Option<SourceConfig> + Send + Sync>,
    history: Arc<HistoryStore>,
    app: tauri::AppHandle,
    running: AtomicBool,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// 启动时随机生成的认证 token（客户端须带 X-Proxy-Token 头）
    token: Mutex<String>,
}

/// 随机 32 位 base36 token（防本地进程/浏览器页面滥用代理）
fn gen_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| {
            let c = rng.gen_range(0..36);
            if c < 10 {
                (b'0' + c as u8) as char
            } else {
                (b'a' + (c - 10) as u8) as char
            }
        })
        .collect()
}

/// 从 SSE 行缓冲取出所有完整行（含 \n，去掉 \r），未完成的尾部留在缓冲里
fn drain_sse_lines(buf: &mut String, mut on_line: impl FnMut(&str)) {
    while let Some(idx) = buf.find('\n') {
        let line: String = buf[..idx].trim_end_matches('\r').to_string();
        buf.drain(..=idx);
        on_line(&line);
    }
}

impl GoProxy {
    pub fn new(
        get_source: Arc<dyn Fn() -> Option<SourceConfig> + Send + Sync>,
        history: Arc<HistoryStore>,
        app: tauri::AppHandle,
    ) -> Self {
        Self {
            inner: Arc::new(GoProxyInner {
                get_source,
                history,
                app,
                running: AtomicBool::new(false),
                task: Mutex::new(None),
                token: Mutex::new(gen_token()),
            }),
        }
    }

    pub fn source(&self) -> Option<SourceConfig> {
        (self.inner.get_source)()
    }

    pub fn port(&self) -> u16 {
        self.inner.port()
    }

    pub fn base_url(&self) -> String {
        self.inner.base_url()
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    /// 启动本地代理（幂等）；每次启动轮换认证 token
    pub async fn start(&self) -> bool {
        if self.is_running() {
            return true;
        }
        // 每次启动轮换 token，旧客户端配置即刻失效
        *self.inner.token.lock().unwrap() = gen_token();
        let port = self.port();
        let inner = self.inner.clone();
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[proxy] 启动失败: {e}");
                return false;
            }
        };
        let handle = tauri::async_runtime::spawn(async move {
            inner.running.store(true, Ordering::SeqCst);
            log::info!("[proxy] 本地代理已启动 http://127.0.0.1:{port}");
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let inner2 = inner.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req| {
                        let i = inner2.clone();
                        async move { i.clone().handle(req).await }
                    });
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await
                    {
                        log::warn!("[proxy] 连接处理失败: {e}");
                    }
                });
            }
        });
        *self.inner.task.lock().unwrap() = Some(handle);
        true
    }

    pub fn stop(&self) {
        self.inner.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.inner.task.lock().unwrap().take() {
            h.abort();
        }
    }

    pub fn status(&self) -> serde_json::Value {
        let port = self.port();
        let token = self.inner.token();
        serde_json::json!({
            "running": self.is_running(),
            "port": port,
            "host": "127.0.0.1",
            "baseUrl": self.base_url(),
            "token": token,
            "hint": format!("将客户端的 API Base URL 设置为 http://127.0.0.1:{port}，并在请求头添加 X-Proxy-Token: {token}（保留 /v1 路径；每次重启代理会轮换 token）"),
        })
    }
}

impl GoProxyInner {
    fn token(&self) -> String {
        self.token.lock().unwrap().clone()
    }

    /// 代理防滥用校验：
    /// - 所有请求必须带 X-Proxy-Token（随机生成，启动时轮换）
    /// - 浏览器来源（带 Origin）必须是本应用 webview，防恶意网页驱动代理产生费用
    fn auth_ok(&self, parts: &hyper::http::request::Parts) -> bool {
        auth_ok(&self.token(), parts)
    }
    fn source(&self) -> Option<SourceConfig> {
        (self.get_source)()
    }

    fn port(&self) -> u16 {
        self.source()
            .and_then(|s| s.port)
            .filter(|p| *p > 0)
            .unwrap_or(12345) as u16
    }

    fn base_url(&self) -> String {
        self.source()
            .and_then(|s| s.base_url)
            .unwrap_or_else(|| "https://opencode.ai/zen/go".to_string())
            .trim_end_matches('/')
            .to_string()
    }

    /// 处理单个代理请求（对齐 main/proxy.js 的 handle）
    async fn handle(self: Arc<Self>, req: Request<Incoming>) -> Result<Response<ProxyBody>, io::Error> {
        let (parts, body) = req.into_parts();
        // 防滥用认证：token + 受信 Origin
        if !self.auth_ok(&parts) {
            return Ok(json_response(
                StatusCode::FORBIDDEN,
                r#"{"error":{"message":"代理需要 X-Proxy-Token 认证（见设置 → 本地代理）"}}"#,
            ));
        }
        // 缓冲请求体（流式累计，上限 10MB；超限即拒绝，不整块缓冲）
        let mut body_bytes: Vec<u8> = Vec::new();
        let mut body = body;
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(f) => {
                    if let Ok(data) = f.into_data() {
                        body_bytes.extend_from_slice(&data);
                        if body_bytes.len() > MAX_BODY_BYTES {
                            return Ok(json_response(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                r#"{"error":{"message":"请求体超过 10MB 上限"}}"#,
                            ));
                        }
                    }
                }
                Err(_) => {
                    return Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        r#"{"error":{"message":"读取请求体失败"}}"#,
                    ))
                }
            }
        }
        let body_bytes = Bytes::from(body_bytes);
        // 请求体里的 model（用于流式 chunk 无 model 字段时）
        let req_model = serde_json::from_str::<Value>(&String::from_utf8_lossy(&body_bytes))
            .ok()
            .and_then(|v| v.get("model").and_then(Value::as_str).map(|s| s.to_string()));

        let accept_sse = parts
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(|a| a.contains("text/event-stream"))
            .unwrap_or(false);

        // 上游 URL = baseUrl + 客户端路径（保留 /v1 等）
        let path = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let target_url = format!("{}{}", self.base_url(), path);

        // 构建上游请求：透传除 hop-by-hop 外的所有头（Authorization 保留；
        // x-proxy-token 是给本代理的认证头，不该转发给上游）
        // 上游超时 60s + 禁止自动重定向；构建失败直接报错（不回退默认 client，避免跟随重定向泄凭据）
        let client = match reqwest::Client::builder()
            .timeout(UPSTREAM_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                log::error!("[proxy] 构建上游 client 失败: {e}");
                return Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    r#"{"error":{"message":"代理内部错误"}}"#,
                ));
            }
        };
        let mut rb = client
            .request(parts.method.clone(), &target_url)
            .body(body_bytes.to_vec());
        for (k, v) in parts.headers.iter() {
            let name = k.as_str().to_lowercase();
            if matches!(
                name.as_str(),
                "host" | "connection" | "content-length" | "transfer-encoding" | "x-proxy-token"
            ) {
                continue;
            }
            if let Ok(s) = v.to_str() {
                rb = rb.header(k.as_str(), s);
            }
        }
        if parts.headers.get("accept").is_none() {
            rb = rb.header("accept", "application/json");
        }

        let upstream = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                log::warn!("[proxy] 转发失败: {e}");
                return Ok(json_response(
                    StatusCode::BAD_GATEWAY,
                    &format!(r#"{{"error":{{"message":"代理转发失败: {e}"}}}}"#),
                ));
            }
        };

        // 是否流式：以上游响应 content-type 为准（客户端 accept 头可能误报）。
        // 只有明确 JSON 才走整包解析；未声明或非 JSON 类型（如 text/plain 的流式服务）
        // 回退到客户端 accept 判断，避免流式响应被整包收集后按行解析漏掉 usage
        let resp_ct = upstream
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let is_stream = if resp_ct.contains("text/event-stream") {
            true
        } else if resp_ct.contains("json") {
            false // 明确 JSON → 非流式，收集完整响应后解析
        } else {
            accept_sse
        };

        let status = upstream.status();
        let mut builder = Response::builder().status(status);
        for (k, v) in upstream.headers() {
            let name = k.as_str().to_lowercase();
            if matches!(name.as_str(), "connection" | "transfer-encoding" | "content-length") {
                continue;
            }
            if let Ok(hv) = HeaderValue::from_str(v.to_str().unwrap_or("")) {
                builder = builder.header(k, hv);
            }
        }

        if is_stream {
            // SSE 流式：边转发边解析 usage（跨 chunk 缓冲未完成行，防止半截 data: 行漏记）
            let model = req_model.clone();
            let this = self.clone();
            let mut line_buf = String::new();
            let stream = upstream.bytes_stream().map(move |chunk| {
                if let Ok(c) = &chunk {
                    this.parse_stream_chunk(c, model.as_deref(), &mut line_buf);
                }
                chunk.map_err(|e| io::Error::other(e.to_string()))
            });
            Ok(builder
                .body(stream_body(stream))
                .unwrap_or_else(|_| Response::new(full_bytes(Bytes::new()))))
        } else {
            // 非流式：收集完整响应后解析
            let bytes = match upstream.bytes().await {
                Ok(b) => b,
                Err(_) => Bytes::new(),
            };
            self.record_from_json(&bytes, req_model.as_deref());
            Ok(builder
                .body(full_bytes(bytes))
                .unwrap_or_else(|_| Response::new(full_bytes(Bytes::new()))))
        }
    }

    /// SSE chunk 逐行解析：跨 chunk 缓冲未完成的 data: 行（TCP 分片可能切断行尾），
    /// 完整行才解析，避免 usage 事件被切开时漏记
    fn parse_stream_chunk(&self, chunk: &Bytes, model: Option<&str>, line_buf: &mut String) {
        line_buf.push_str(&String::from_utf8_lossy(chunk));
        drain_sse_lines(line_buf, |line| {
            let line = line.trim();
            if !line.starts_with("data:") {
                return;
            }
            let payload = line[5..].trim();
            if payload.is_empty() || payload == "[DONE]" {
                return;
            }
            if let Ok(v) = serde_json::from_str::<Value>(payload) {
                self.extract_and_record(&v, model);
            }
        });
        // 防异常流无限增长（正常 SSE 行很短；1MB 上限后丢弃未完成行并告警）
        if line_buf.len() > 1024 * 1024 {
            log::warn!("[proxy] SSE 行缓冲超过 1MB，丢弃未完成行");
            line_buf.clear();
        }
    }

    fn record_from_json(&self, buf: &Bytes, model: Option<&str>) {
        if let Ok(v) = serde_json::from_slice::<Value>(buf) {
            self.extract_and_record(&v, model);
        }
    }

    fn extract_and_record(&self, j: &Value, req_model: Option<&str>) {
        if !j.is_object() {
            return;
        }
        let model = j
            .get("model")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .or_else(|| req_model.map(|s| s.to_string()));
        if let Some(u) = j.get("usage") {
            self.record_usage(u, model.as_deref());
            return;
        }
        // chat.completion.chunk / message 内嵌 usage
        if let Some(choice) = j
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        {
            if let Some(msg) = choice.get("message").or_else(|| choice.get("delta")) {
                if let Some(u) = msg.get("usage") {
                    self.record_usage(u, model.as_deref());
                }
            }
        }
    }

    fn record_usage(&self, u: &Value, model: Option<&str>) {
        let num = |keys: &[&str]| -> Option<f64> {
            for k in keys {
                if let Some(n) = u.get(*k).and_then(Value::as_f64) {
                    return Some(n);
                }
            }
            None
        };
        let input = num(&["input_tokens", "inputTokens", "prompt_tokens"]);
        let output = num(&["output_tokens", "outputTokens", "completion_tokens"]);
        let total = num(&["total_tokens", "totalTokens"]).or(match (input, output) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        });
        let Some(total) = total else { return };
        if !total.is_finite() || total <= 0.0 {
            return;
        }
        let Some(source) = self.source() else { return };
        let model = model.unwrap_or("(unknown)").to_string();
        let price_map = source_price_map(&source);
        let cost = cost_for(&model, input.unwrap_or(0.0), output.unwrap_or(0.0), &price_map);
        self.history.add_records(
            &source.id,
            vec![crate::history::RecordIn {
                ts: Some(crate::util::now_ms()),
                model: Some(model),
                provider: None,
                input,
                output,
                total,
                cache_read: None,
                cost,
            }],
            true,
        );
        let _ = self.app.emit(
            "data:update",
            DataUpdate {
                source_id: Some(source.id.clone()),
                count: Some(1),
                ..Default::default()
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::http::Request;

    fn parts_with(headers: &[(&str, &str)]) -> hyper::http::request::Parts {
        let mut req = Request::builder().uri("/v1/chat/completions");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.body(()).unwrap().into_parts().0
    }

    #[test]
    fn proxy_auth_requires_token() {
        assert!(!auth_ok("secret", &parts_with(&[])), "无 token 应拒绝");
        assert!(!auth_ok("secret", &parts_with(&[("x-proxy-token", "wrong")])), "错误 token 应拒绝");
        assert!(auth_ok("secret", &parts_with(&[("x-proxy-token", "secret")])), "正确 token 应通过");
    }

    #[test]
    fn proxy_auth_rejects_untrusted_origin() {
        // 恶意网页：即使带正确 token，非受信 Origin 也拒绝
        assert!(!auth_ok("secret", &parts_with(&[
            ("x-proxy-token", "secret"),
            ("origin", "http://evil.com"),
        ])));
        // 本应用 webview 的 Origin 允许
        assert!(auth_ok("secret", &parts_with(&[
            ("x-proxy-token", "secret"),
            ("origin", "http://ipc.localhost"),
        ])));
    }

    #[test]
    fn sse_line_buffer_reassembles_split_data_line() {
        // 模拟 usage 的 data: 行被 TCP 分片切成两半：半截行留在缓冲，补全后恰好取出一行
        let mut buf = String::new();
        let mut lines: Vec<String> = Vec::new();
        buf.push_str(r#"data: {"model":"m1","usage":{"total_tokens""#);
        drain_sse_lines(&mut buf, |l| lines.push(l.to_string()));
        assert!(lines.is_empty(), "无换行符时不应取出任何行");
        assert!(!buf.is_empty(), "半截行应保留在缓冲中");
        buf.push_str(": 42,\"input_tokens\":10,\"output_tokens\":32}}\n");
        drain_sse_lines(&mut buf, |l| lines.push(l.to_string()));
        assert_eq!(lines.len(), 1, "跨 chunk 的 data 行应恰好组成一行");
        assert!(lines[0].starts_with(r#"data: {"model":"m1""#));
        assert!(buf.is_empty(), "完整行取出后缓冲应清空");
    }
}

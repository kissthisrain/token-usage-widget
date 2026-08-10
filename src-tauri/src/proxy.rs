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
use tauri::Emitter;

type ProxyBody = UnsyncBoxBody<Bytes, io::Error>;

fn full_bytes(b: Bytes) -> ProxyBody {
    UnsyncBoxBody::new(Full::new(b).map_err(|e| io::Error::new(io::ErrorKind::Other, e)))
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

pub struct GoProxy {
    inner: Arc<GoProxyInner>,
}

struct GoProxyInner {
    get_source: Arc<dyn Fn() -> Option<SourceConfig> + Send + Sync>,
    history: Arc<HistoryStore>,
    app: tauri::AppHandle,
    running: AtomicBool,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
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

    /// 启动本地代理（幂等）
    pub async fn start(&self) -> bool {
        if self.is_running() {
            return true;
        }
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
        serde_json::json!({
            "running": self.is_running(),
            "port": port,
            "host": "127.0.0.1",
            "baseUrl": self.base_url(),
            "hint": format!("将客户端的 API Base URL 设置为 http://127.0.0.1:{port}（保留 /v1 路径）"),
        })
    }
}

impl GoProxyInner {
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
        // 缓冲请求体
        let body_bytes = match body.collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => {
                return Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    r#"{"error":{"message":"读取请求体失败"}}"#,
                ))
            }
        };
        // 请求体里的 model（用于流式 chunk 无 model 字段时）
        let req_model = serde_json::from_str::<Value>(&String::from_utf8_lossy(&body_bytes))
            .ok()
            .and_then(|v| v.get("model").and_then(Value::as_str).map(|s| s.to_string()));

        let is_stream = parts
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(|a| a.contains("text/event-stream"))
            .unwrap_or(false);

        // 上游 URL = baseUrl + 客户端路径（保留 /v1 等）
        let path = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let target_url = format!("{}{}", self.base_url(), path);

        // 构建上游请求：透传除 hop-by-hop 外的所有头（Authorization 保留）
        let mut rb = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
            .request(parts.method.clone(), &target_url)
            .body(body_bytes.to_vec());
        for (k, v) in parts.headers.iter() {
            let name = k.as_str().to_lowercase();
            if matches!(
                name.as_str(),
                "host" | "connection" | "content-length" | "transfer-encoding"
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
            // SSE 流式：边转发边解析 usage（对齐 parseStreamChunk）
            let model = req_model.clone();
            let this = self.clone();
            let stream = upstream.bytes_stream().map(move |chunk| {
                if let Ok(c) = &chunk {
                    this.parse_stream_chunk(c, model.as_deref());
                }
                chunk.map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
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

    /// SSE chunk 逐行解析（容忍半截 JSON）
    fn parse_stream_chunk(&self, chunk: &Bytes, model: Option<&str>) {
        let text = String::from_utf8_lossy(chunk);
        for line in text.lines() {
            if !line.starts_with("data:") {
                continue;
            }
            let payload = line[5..].trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(payload) {
                self.extract_and_record(&v, model);
            }
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
        let total = num(&["total_tokens", "totalTokens"]).or_else(|| match (input, output) {
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

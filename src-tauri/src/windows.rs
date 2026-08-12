// 窗口管理：悬浮小组件窗口 + 设置窗口（对齐 main/window.js）

use crate::config::{ConfigStore, WindowBounds};
use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};
use tauri::window::{Color, Effect, EffectState, EffectsBuilder};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

const WIDGET_LABEL: &str = "widget";
const SETTINGS_LABEL: &str = "settings";
/// 小组件窗口设计宽度（逻辑像素），所有 setSize 恒用它，避免混合 DPI 下宽度漂移
const WIDGET_WIDTH: f64 = 440.0;
/// 精简界面尺寸（逻辑px）
const MINI_W: f64 = 190.0;
const MINI_H: f64 = 280.0;

/// 是否支持 Win11 亚克力毛玻璃（build >= 22621）
pub fn can_acrylic() -> bool {
    static CACHE: Lazy<bool> = Lazy::new(|| {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
                "/v",
                "CurrentBuildNumber",
            ])
            .output();
        if let Ok(o) = out {
            let s = String::from_utf8_lossy(&o.stdout);
            if let Some(build) = s.split_whitespace().last() {
                if let Ok(n) = build.parse::<i64>() {
                    return n >= 22621;
                }
            }
        }
        false
    });
    *CACHE
}

pub struct WindowMgr {
    app: AppHandle,
    config: Arc<ConfigStore>,
    /// 当前视图：mini（精简界面）/ main（主界面）
    view: Mutex<String>,
    /// 主视图位置（切到精简前保存，切回时恢复，避免窗口漂移）
    main_pos: Mutex<Option<PhysicalPosition<i32>>>,
}

impl WindowMgr {
    pub fn new(app: AppHandle, config: Arc<ConfigStore>) -> Self {
        Self {
            app,
            config,
            view: Mutex::new("main".to_string()),
            main_pos: Mutex::new(None),
        }
    }

    pub fn widget(&self) -> Option<WebviewWindow> {
        self.app.get_webview_window(WIDGET_LABEL)
    }

    /// 展开/切换为精简界面（mini）：悬浮 mini，位置保持当前
    pub fn show_mini(&self) {
        let Some(win) = self.widget() else { return };
        let Ok(scale) = win.scale_factor() else { return };
        let mini_size = PhysicalSize::new((MINI_W * scale) as u32, (MINI_H * scale) as u32);
        // 保存主视图位置（切回时恢复）；悬浮精简界面位置保持当前
        if let Ok(p) = win.outer_position() {
            *self.main_pos.lock().unwrap() = Some(p);
        }
        let _ = win.set_size(mini_size);
        *self.view.lock().unwrap() = "mini".to_string();
        let _ = self
            .app
            .emit("app:dock", serde_json::json!({ "docked": false, "view": "mini" }));
        log::info!("[win] 切换精简界面(悬浮)");
    }

    /// 切换为主界面：窗口在当前屏幕居中展示
    pub fn show_main(&self) {
        let Some(win) = self.widget() else { return };
        let cfg = self.config.get_general();
        let Ok(scale) = win.scale_factor() else { return };
        // 从精简界面切回：宽 440，高度保持当前（前端会按内容重新自适应）
        let cur_h = win.outer_size().map(|s| s.height as f64).unwrap_or(700.0 * scale);
        let (w, h) = (WIDGET_WIDTH * scale, cur_h);
        let _ = win.set_size(PhysicalSize::new(w, h));
        // 恢复到切到精简前的主视图位置（避免切回时窗口漂移）
        if let Some(p) = self.main_pos.lock().unwrap().take() {
            let _ = win.set_position(p);
        }
        let _ = win.set_always_on_top(cfg.always_on_top);
        *self.view.lock().unwrap() = "main".to_string();
        let _ = self
            .app
            .emit("app:dock", serde_json::json!({ "docked": false, "view": "main" }));
        log::info!("[win] 切换主界面");
    }

    /// 当前是否为精简界面
    pub fn is_mini(&self) -> bool {
        self.view.lock().unwrap().as_str() == "mini"
    }

    /// 主悬浮窗口
    pub fn create_widget(&self) {
        if self.widget().is_some() {
            return;
        }
        let cfg = self.config.get();
        let bounds = cfg.window_bounds.clone();
        let pinned = cfg.general.always_on_top;
        let acrylic = can_acrylic();

        let mut builder = WebviewWindowBuilder::new(&self.app, WIDGET_LABEL, WebviewUrl::App("index.html".into()))
            .title("Token 用量")
            .inner_size(440.0, 700.0)
            .decorations(false)
            .transparent(true)
            // 固定大小（用户不可拖边框调整）；视图切换的 set_size 仍由程序控制
            .resizable(false)
            .maximizable(false)
            .fullscreen(false)
            .skip_taskbar(true)
            .shadow(acrylic)
            .always_on_top(pinned)
            .visible(false)
            .background_color(Color(16, 18, 22, 255));
        if let Some(b) = &bounds {
            if let (Some(x), Some(y)) = (b.x, b.y) {
                builder = builder.position(x, y);
            }
        }
        let win = match builder.build() {
            Ok(w) => w,
            Err(e) => {
                log::error!("创建小组件窗口失败: {e}");
                return;
            }
        };
        if acrylic {
            // Win11 亚克力毛玻璃（DWM 效果，对齐 Electron backgroundMaterial: 'acrylic'）
            if let Err(e) = win.set_effects(
                EffectsBuilder::new()
                    .effect(Effect::Acrylic)
                    .state(EffectState::Active)
                    .color(Color(0, 0, 0, 0))
                    .build(),
            ) {
                log::warn!("设置亚克力材质失败: {e}");
            }
        }
        if pinned {
            let _ = win.set_always_on_top(true);
        }

        // 位置变化 → 持久化 windowBounds（逻辑像素，与 Electron 的 getBounds 口径一致）
        let cfg_store = self.config.clone();
        let win_for_event = win.clone();
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::Moved(pos) = event {
                let scale = win_for_event.scale_factor().unwrap_or(1.0);
                let (x, y) = to_logical(*pos, scale);
                let mut c = cfg_store.get();
                c.window_bounds = Some(WindowBounds { x: Some(x), y: Some(y) });
                *cfg_store.data.lock().unwrap() = c;
                cfg_store.save();
                // 跨屏高度适配：新屏可用高度小于当前窗口高度时自动缩小
                // （底部内容靠内部滚动可达；只缩不放，避免 resize 死循环）
                // 全部用逻辑像素比较与设置；宽度恒为 WIDGET_WIDTH（混合 DPI 下不反推物理宽）
                if let Ok(Some(m)) = win_for_event.current_monitor() {
                    let max_h_logical = (m.size().height as f64 - 24.0) / scale;
                    if let Ok(cur) = win_for_event.outer_size() {
                        let cur_h_logical = cur.height as f64 / scale;
                        if cur_h_logical > max_h_logical {
                            log::info!(
                                "[win] 跨屏高度适配: cur_h={} max_h={} (逻辑px, scale={})",
                                cur_h_logical,
                                max_h_logical,
                                scale
                            );
                            let _ = win_for_event
                                .set_size(LogicalSize::new(WIDGET_WIDTH, max_h_logical));
                        }
                    }
                }
            }
        });

        self.restore_on_screen(&win, bounds);
        // 不立即 show：等前端加载完成并自适应高度后，由 win_ready 通知显示
        // （避免 WebView2 未就绪时窗口闪深色底框、以及高度从 700 撑到内容高度）
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            // 兜底：前端加载失败/超时也要显示窗口（防窗口永不出现）
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            if let Some(w) = app.get_webview_window(WIDGET_LABEL) {
                if !w.is_visible().unwrap_or(false) {
                    let _ = w.show();
                }
            }
        });
    }

    /// 前端就绪后显示窗口（首次加载完成后由 win_ready 调用）
    pub fn show_ready(&self) {
        if let Some(win) = self.widget() {
            if !win.is_visible().unwrap_or(false) {
                let _ = win.show();
            }
        }
    }

    fn restore_on_screen(&self, win: &WebviewWindow, bounds: Option<WindowBounds>) {
        let Some(b) = bounds else { return };
        let (Some(x), Some(y)) = (b.x, b.y) else { return };
        // 在所有显示器中查找包含该保存位置的显示器（仅查"当前显示器"在跨屏时会误判，
        // 导致窗口无法恢复到另一块屏幕上的旧位置）
        let on_screen = win
            .available_monitors()
            .map(|mons| {
                mons.iter().any(|m| {
                    let p = m.position();
                    let s = m.size();
                    x >= p.x as f64 - 100.0
                        && x <= p.x as f64 + s.width as f64 - 50.0
                        && y >= p.y as f64 - 20.0
                        && y <= p.y as f64 + s.height as f64 - 50.0
                })
            })
            .unwrap_or(false);
        if on_screen {
            let _ = win.set_position(LogicalPosition::new(x, y));
        }
    }

    /// 窗口高度自适应内容（上限为屏幕高度，对齐 resizeToContent）
    /// 宽度恒为设计值 WIDGET_WIDTH（逻辑像素），不从物理尺寸反推——混合 DPI 下
    /// outer_size 的物理宽会被 DPI 上下文污染导致宽度累积增长
    pub fn resize_to_content(&self, height: f64) {
        let Some(win) = self.widget() else { return };
        let Ok(scale) = win.scale_factor() else { return };
        let Ok(cur_phys) = win.outer_size() else { return };
        // 屏幕可用高度（转逻辑像素，与前端传入的 h 同单位）
        let max_logical = match win.current_monitor() {
            Ok(Some(m)) => (m.size().height as f64 - 24.0) / scale,
            _ => 700.0,
        };
        let h = height.clamp(320.0, max_logical);
        let cur_h_logical = cur_phys.height as f64 / scale;
        log::info!(
            "[win] resize_to_content: h={} cur_h={} max={} scale={}",
            h,
            cur_h_logical,
            max_logical,
            scale
        );
        if (cur_h_logical - h).abs() > 3.0 {
            let _ = win.set_size(LogicalSize::new(WIDGET_WIDTH, h));
        }
    }

    /// 不透明度交给渲染层 CSS 实现，这里推送数值（对齐 applyOpacity）
    pub fn apply_opacity(&self) {
        let o = self.config.get_general().opacity;
        let _ = self.app.emit("app:opacity", o.clamp(0.4, 1.0));
    }

    pub fn toggle_widget(&self) {
        let Some(win) = self.widget() else {
            self.create_widget();
            return;
        };
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }

    pub fn show_widget(&self) {
        let Some(win) = self.widget() else {
            self.create_widget();
            return;
        };
        if !win.is_visible().unwrap_or(false) {
            let _ = win.show();
        }
    }

    /// 钉在前排：切换置顶并持久化到配置
    pub fn set_pinned(&self, v: bool) -> bool {
        if let Some(win) = self.widget() {
            let _ = win.set_always_on_top(v);
        }
        let mut c = self.config.get();
        c.general.always_on_top = v;
        *self.config.data.lock().unwrap() = c;
        self.config.save();
        v
    }

    pub fn get_pinned(&self) -> bool {
        self.config.get_general().always_on_top
    }

    /// 设置窗口
    pub fn open_settings(&self) {
        if let Some(win) = self.app.get_webview_window(SETTINGS_LABEL) {
            let _ = win.show();
            let _ = win.set_focus();
            return;
        }
        log::info!("[windows] 创建设置窗口…");
        let r = WebviewWindowBuilder::new(&self.app, SETTINGS_LABEL, WebviewUrl::App("settings.html".into()))
            .title("设置")
            .inner_size(780.0, 680.0)
            .min_inner_size(640.0, 520.0)
            .decorations(false)
            .visible(true)
            .background_color(Color(16, 18, 22, 255))
            .build();
        log::info!("[windows] 设置窗口创建完成: {:?}", r.is_ok());
    }

    pub fn quit(&self) {
        self.app.exit(0);
    }
}

/// 物理位置 → 逻辑位置
fn to_logical(p: PhysicalPosition<i32>, scale: f64) -> (f64, f64) {
    (p.x as f64 / scale, p.y as f64 / scale)
}

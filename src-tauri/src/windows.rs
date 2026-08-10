// 窗口管理：悬浮小组件窗口 + 设置窗口（对齐 main/window.js）

use crate::config::{ConfigStore, WindowBounds};
use once_cell::sync::Lazy;
use std::sync::Arc;
use tauri::window::{Color, Effect, EffectState, EffectsBuilder};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder,
};

const WIDGET_LABEL: &str = "widget";
const SETTINGS_LABEL: &str = "settings";

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
}

impl WindowMgr {
    pub fn new(app: AppHandle, config: Arc<ConfigStore>) -> Self {
        Self { app, config }
    }

    pub fn widget(&self) -> Option<WebviewWindow> {
        self.app.get_webview_window(WIDGET_LABEL)
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
            .resizable(false)
            .maximizable(false)
            .fullscreen(false)
            .skip_taskbar(true)
            .shadow(acrylic)
            .always_on_top(pinned)
            .visible(false)
            .background_color(Color(0, 0, 0, 0));
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
            }
        });

        self.restore_on_screen(&win, bounds);
        let _ = win.show();
    }

    fn restore_on_screen(&self, win: &WebviewWindow, bounds: Option<WindowBounds>) {
        let Some(b) = bounds else { return };
        let (Some(x), Some(y)) = (b.x, b.y) else { return };
        let on_screen = match win.current_monitor() {
            Ok(Some(m)) => {
                let p = m.position();
                let s = m.size();
                x >= p.x as f64 - 100.0
                    && x <= p.x as f64 + s.width as f64 - 50.0
                    && y >= p.y as f64 - 20.0
                    && y <= p.y as f64 + s.height as f64 - 50.0
            }
            _ => false,
        };
        if on_screen {
            let _ = win.set_position(LogicalPosition::new(x, y));
        }
    }

    /// 窗口高度自适应内容（上限为屏幕高度，对齐 resizeToContent）
    pub fn resize_to_content(&self, height: f64) {
        let Some(win) = self.widget() else { return };
        let Ok(scale) = win.scale_factor() else { return };
        let Ok(cur_phys) = win.outer_size() else { return };
        let max = match win.current_monitor() {
            Ok(Some(m)) => m.size().height as f64 - 24.0,
            _ => 700.0,
        };
        let h = height.clamp(320.0, max);
        let w_logical = cur_phys.width as f64 / scale;
        let cur_h_logical = cur_phys.height as f64 / scale;
        if (cur_h_logical - h).abs() > 3.0 {
            let _ = win.set_size(LogicalSize::new(w_logical, h));
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

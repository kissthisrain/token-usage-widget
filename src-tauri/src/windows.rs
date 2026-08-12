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
/// 贴边收缩：距边缘小于该距离（逻辑px）且拖动停止时触发收缩
const DOCK_THRESHOLD: f64 = 40.0;
/// 贴边收缩：收缩条被拖离边缘超过该距离（逻辑px）时脱离贴边
/// （值取小，保证拖动时能快速识别并释放，避免"拖不动"）
const DOCK_UNDOCK_DIST: f64 = 25.0;
/// 贴边小条尺寸（逻辑px）：贴左右边 → 竖条；贴顶底边 → 横条
/// 竖条 56x100 ≈ 1:1.8 紧凑比例（原 56x180 过修长）；横条相应压扁
const DOCK_V_W: f64 = 56.0;
const DOCK_V_H: f64 = 100.0;
const DOCK_H_W: f64 = 150.0;
const DOCK_H_H: f64 = 42.0;
/// 精简界面尺寸（逻辑px）
const MINI_W: f64 = 190.0;
const MINI_H: f64 = 280.0;

/// 贴边收缩状态
#[derive(Clone)]
struct DockState {
    edge: String, // "left" | "right" | "top" | "bottom"
    /// 贴边小条位置（物理px）
    dock_pos: PhysicalPosition<i32>,
    /// 贴边小条尺寸（物理px）
    bar_size: PhysicalSize<u32>,
    /// 收缩前的完整窗口尺寸（物理px）
    prev_size: PhysicalSize<u32>,
}

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
    dock: Mutex<Option<DockState>>,
    /// 当前视图：bar（贴边小条）/ mini（精简界面）/ main（主界面）
    view: Mutex<String>,
}

impl WindowMgr {
    pub fn new(app: AppHandle, config: Arc<ConfigStore>) -> Self {
        Self {
            app,
            config,
            dock: Mutex::new(None),
            view: Mutex::new("main".to_string()),
        }
    }

    pub fn widget(&self) -> Option<WebviewWindow> {
        self.app.get_webview_window(WIDGET_LABEL)
    }

    /// 启动贴边检测轮询：拖动停止后检查窗口是否贴边 → 自动收缩
    pub fn start_dock_watcher(self: &Arc<Self>) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
            interval.tick().await;
            loop {
                interval.tick().await;
                this.dock_tick();
            }
        });
    }

    /// 贴边检测轮询（每 200ms）
    fn dock_tick(&self) {
        let Some(win) = self.widget() else { return };
        let Ok(scale) = win.scale_factor() else { return };
        let Ok(pos) = win.outer_position() else { return };
        let Ok(size) = win.outer_size() else { return };
        let Ok(Some(m)) = win.current_monitor() else { return };
        let cfg = self.config.get_general();
        let docked = self.dock.lock().unwrap().is_some();

        if !cfg.dock_enabled {
            if docked {
                self.show_main();
            }
            return;
        }

        if !docked {
            // 距各边缘距离（逻辑px）
            let mpos = m.position();
            let msize = m.size();
            let left_d = (pos.x - mpos.x) as f64 / scale;
            let right_d = (mpos.x + msize.width as i32 - (pos.x + size.width as i32)) as f64 / scale;
            let top_d = (pos.y - mpos.y) as f64 / scale;
            let bottom_d = (mpos.y + msize.height as i32 - (pos.y + size.height as i32)) as f64 / scale;
            let min_d = left_d.min(right_d).min(top_d).min(bottom_d);
            if min_d < DOCK_THRESHOLD {
                let edge = if left_d == min_d {
                    "left"
                } else if right_d == min_d {
                    "right"
                } else if top_d == min_d {
                    "top"
                } else {
                    "bottom"
                };
                self.dock(edge, pos, size, scale);
            }
        } else {
            // 已收缩：若被拖离边缘 → 释放（恢复完整尺寸，位置跟随用户，清空 dock 状态）
            let st = self.dock.lock().unwrap().clone().unwrap();
            let dx = (pos.x - st.dock_pos.x).abs() as f64 / scale;
            let dy = (pos.y - st.dock_pos.y).abs() as f64 / scale;
            if dx > DOCK_UNDOCK_DIST || dy > DOCK_UNDOCK_DIST {
                self.release_dock();
            }
        }
    }

    /// 收缩到贴边小条（悬停展开/收回等操作也走这里）
    fn dock(&self, edge: &str, pos: PhysicalPosition<i32>, size: PhysicalSize<u32>, scale: f64) {
        let Some(win) = self.widget() else { return };
        let cfg = self.config.get_general();
        let (dw, dh) = match edge {
            "left" | "right" => (DOCK_V_W, DOCK_V_H),
            _ => (DOCK_H_W, DOCK_H_H),
        };
        let Ok(Some(m)) = win.current_monitor() else { return };
        let mpos = m.position();
        // 贴边位置：贴左/右 → x 贴边、y 保持当前；贴顶/底 → y 贴边、x 保持当前
        let dock_pos = match edge {
            "left" => PhysicalPosition::new(mpos.x, pos.y),
            "right" => PhysicalPosition::new(mpos.x + m.size().width as i32 - (dw * scale) as i32, pos.y),
            "top" => PhysicalPosition::new(pos.x, mpos.y),
            _ => PhysicalPosition::new(pos.x, mpos.y + m.size().height as i32 - (dh * scale) as i32),
        };
        let bar_size = PhysicalSize::new((dw * scale) as u32, (dh * scale) as u32);
        log::info!(
            "[win] dock: edge={} scale={} monitor=({},{}) {}x{} dock_pos=({},{}) bar_size={}x{}",
            edge,
            scale,
            mpos.x,
            mpos.y,
            m.size().width,
            m.size().height,
            dock_pos.x,
            dock_pos.y,
            bar_size.width,
            bar_size.height
        );
        let _ = win.set_size(bar_size);
        let _ = win.set_position(dock_pos);
        // 日志确认设置后的实际值
        if let Ok(p) = win.outer_position() {
            if let Ok(s) = win.outer_size() {
                log::info!(
                    "[win] dock 后实际: pos=({},{}) size={}x{}",
                    p.x,
                    p.y,
                    s.width,
                    s.height
                );
            }
        }
        if cfg.dock_always_on_top {
            let _ = win.set_always_on_top(true);
        }
        *self.dock.lock().unwrap() = Some(DockState {
            edge: edge.to_string(),
            dock_pos,
            bar_size,
            prev_size: size,
        });
        *self.view.lock().unwrap() = "bar".to_string();
        log::info!("[win] 贴边收缩: edge={edge}");
        let _ = self
            .app
            .emit("app:dock", serde_json::json!({ "docked": true, "view": "bar", "edge": edge }));
    }

    /// 展开/切换为精简界面（mini）：
    /// - 从贴边小条悬停展开：mini 在贴边处展开（保留 dock 状态，移开可收回）
    /// - 从主界面按钮切换：悬浮 mini，位置保持当前
    pub fn show_mini(&self) {
        let Some(win) = self.widget() else { return };
        let Ok(scale) = win.scale_factor() else { return };
        let mini_size = PhysicalSize::new((MINI_W * scale) as u32, (MINI_H * scale) as u32);
        if let Some(st) = self.dock.lock().unwrap().clone() {
            // 从贴边小条展开：mini 紧贴边缘，垂直/水平对齐 bar 中心
            let pos = self.mini_pos(&st, scale);
            let _ = win.set_size(mini_size);
            let _ = win.set_position(pos);
            *self.view.lock().unwrap() = "mini".to_string();
            let _ = self.app.emit(
                "app:dock",
                serde_json::json!({ "docked": true, "view": "mini", "edge": st.edge }),
            );
            log::info!("[win] 展开精简界面(贴边)");
        } else {
            // 从主界面切换：悬浮 mini，位置保持
            let _ = win.set_size(mini_size);
            *self.view.lock().unwrap() = "mini".to_string();
            let _ = self
                .app
                .emit("app:dock", serde_json::json!({ "docked": false, "view": "mini" }));
            log::info!("[win] 切换精简界面(悬浮)");
        }
    }

    /// 精简界面在贴边处的展开位置（对齐 bar 中心，并 clamp 在屏幕内）
    fn mini_pos(&self, st: &DockState, scale: f64) -> PhysicalPosition<i32> {
        let Some(win) = self.widget() else { return st.dock_pos };
        let Ok(Some(m)) = win.current_monitor() else { return st.dock_pos };
        let mpos = m.position();
        let msize = m.size();
        let mw = (MINI_W * scale) as i32;
        let mh = (MINI_H * scale) as i32;
        let bar_w = st.bar_size.width as i32;
        let bar_h = st.bar_size.height as i32;
        let clamp_center = |bar: i32, bar_len: i32, mini_len: i32, min: i32, max: i32| -> i32 {
            (bar + (bar_len - mini_len) / 2).clamp(min, max - mini_len)
        };
        match st.edge.as_str() {
            "right" => PhysicalPosition::new(
                mpos.x + msize.width as i32 - mw,
                clamp_center(st.dock_pos.y, bar_h, mh, mpos.y, mpos.y + msize.height as i32),
            ),
            "left" => PhysicalPosition::new(
                mpos.x,
                clamp_center(st.dock_pos.y, bar_h, mh, mpos.y, mpos.y + msize.height as i32),
            ),
            "top" => PhysicalPosition::new(
                clamp_center(st.dock_pos.x, bar_w, mw, mpos.x, mpos.x + msize.width as i32),
                mpos.y,
            ),
            _ => PhysicalPosition::new(
                clamp_center(st.dock_pos.x, bar_w, mw, mpos.x, mpos.x + msize.width as i32),
                mpos.y + msize.height as i32 - mh,
            ),
        }
    }

    /// 切换为主界面：解除贴边，窗口在当前屏幕居中展示
    pub fn show_main(&self) {
        let Some(win) = self.widget() else { return };
        let st = self.dock.lock().unwrap().take();
        let cfg = self.config.get_general();
        let Ok(scale) = win.scale_factor() else { return };
        let (w, h) = match &st {
            // 从贴边态切回：恢复收缩前尺寸
            Some(s) => (s.prev_size.width as f64, s.prev_size.height as f64),
            // 从悬浮 mini 切回：宽 440，高度保持当前
            None => {
                let cur_h = win.outer_size().map(|s| s.height as f64).unwrap_or(700.0 * scale);
                (WIDGET_WIDTH * scale, cur_h)
            }
        };
        let _ = win.set_size(PhysicalSize::new(w, h));
        let _ = win.set_always_on_top(cfg.always_on_top);
        *self.view.lock().unwrap() = "main".to_string();
        // 当前屏幕居中
        if let Ok(Some(m)) = win.current_monitor() {
            let mpos = m.position();
            let msize = m.size();
            let cx = mpos.x + (msize.width as i32 - w as i32) / 2;
            let cy = mpos.y + (msize.height as i32 - h as i32) / 2;
            let _ = win.set_position(PhysicalPosition::new(cx, cy));
        }
        let _ = self
            .app
            .emit("app:dock", serde_json::json!({ "docked": false, "view": "main" }));
        log::info!("[win] 切换主界面(居中)");
    }

    /// 拖动脱离：清空 dock 状态，位置跟随用户（绝不强制拉回）。
    /// 若当前是精简界面 → 变为「悬浮精简界面」继续自由拖动；若是小条 → 恢复主界面
    fn release_dock(&self) {
        let Some(win) = self.widget() else { return };
        let Some(st) = self.dock.lock().unwrap().clone() else { return };
        let cfg = self.config.get_general();
        *self.dock.lock().unwrap() = None;
        let view = self.view.lock().unwrap().clone();
        let _ = win.set_always_on_top(cfg.always_on_top);
        if view == "mini" {
            // 精简界面脱离贴边：保持 mini 尺寸，变悬浮态自由拖动
            let _ = self
                .app
                .emit("app:dock", serde_json::json!({ "docked": false, "view": "mini" }));
            log::info!("[win] 精简界面脱离(悬浮)");
        } else {
            // 贴边小条脱离：恢复主界面尺寸
            let _ = win.set_size(st.prev_size);
            *self.view.lock().unwrap() = "main".to_string();
            let _ = self
                .app
                .emit("app:dock", serde_json::json!({ "docked": false, "view": "main" }));
            log::info!("[win] 贴边脱离");
        }
    }

    /// 悬停移开：按上次贴边方向收回贴边小条（窗口已被拖离贴边处则不收回）
    pub fn dock_again(&self) {
        let Some(win) = self.widget() else { return };
        let Some(st) = self.dock.lock().unwrap().clone() else { return };
        let Ok(pos) = win.outer_position() else { return };
        let Ok(scale) = win.scale_factor() else { return };
        let dx = (pos.x - st.dock_pos.x).abs() as f64 / scale;
        let dy = (pos.y - st.dock_pos.y).abs() as f64 / scale;
        if dx > DOCK_UNDOCK_DIST * 2.0 || dy > DOCK_UNDOCK_DIST * 2.0 {
            return; // 用户已拖离边缘，不强制拉回
        }
        let cfg = self.config.get_general();
        let _ = win.set_size(st.bar_size);
        let _ = win.set_position(st.dock_pos);
        if cfg.dock_always_on_top {
            let _ = win.set_always_on_top(true);
        }
        *self.view.lock().unwrap() = "bar".to_string();
        let _ = self.app.emit(
            "app:dock",
            serde_json::json!({ "docked": true, "view": "bar", "edge": st.edge }),
        );
        log::info!("[win] 贴边收回");
    }

    pub fn is_docked(&self) -> bool {
        self.dock.lock().unwrap().is_some()
    }

    /// 当前是否为精简界面（悬浮或贴边展开）
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
        let _ = win.show();
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

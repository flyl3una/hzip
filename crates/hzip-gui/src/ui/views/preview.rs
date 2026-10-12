//! 预览独立窗口（T-G12；06 §4.7）。
//!
//! > **⚠ 当前未接通（用户口径 2026-09-28）**：内置文本/图片查看器**暂时停用**
//! > ——双击文件一律「物化 + 指定程序/系统默认程序打开」（见
//! > `shell::AppShell::open_selection_entry`），本模块与 `PreviewPayload`/
//! > `PreviewEvent` 保留代码待恢复，**当前无调用方**。恢复时把
//! > `open_selection_entry` 的打开分支改回「按 core Auto 判定分流」即可
//! > （历史实现见 git 记录 / 设计稿 11 §4.3、§9.7）。
//!
//! 三视图（文本/图片/HEX）+ 外部打开 + 截断提示。独立于主窗口生命周期
//! （主窗口关 → 预览关，归 T-G30 编排）。数据全部来自 [`PreviewPayload`]
//! （core 读取归 T-G30；本卡不读档案）。
//!
//! # 渲染纪律（T-G12 §3.2）
//!
//! - 文本/HEX 一律 `uniform_list` 虚拟滚动（02 §13.2 性能红线：禁止全量渲染）；
//! - 图片 `img(src).object_fit(ObjectFit::Contain)` contain 缩放居中；
//! - RGBA8 → `RenderImage`（BGRA swap，Spike#10 路径）**在载荷构造时一次做完**
//!   （`PreviewPayload.image` 直接持 `Arc<RenderImage>`；审计 B16：渲染帧零拷贝）；
//! - 解码失败 → 占位「无法解码」文案（不 panic）；
//! - 视图切换态在窗口实体内部（`Entity<PreviewState>`），不发回主窗口。
//!
//! # 同 entry 去重（登记：模块级策略）
//!
//! 按 `entry_name` 记录已开窗口句柄（`AnyWindowHandle` + `update` 判活——
//! handle 指向已关窗口时 `update` 返回 Err → 从注册表移除）；重复 open →
//! 聚焦已有窗口并替换 payload（不重建窗口）。

use crate::ui::constant::{MONO_ROW_H, PREVIEW_WINDOW_H, PREVIEW_WINDOW_W};
use crate::ui::spacing::{component_size, sp};
use std::sync::{Arc, Mutex};

use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, Context, IntoElement, ObjectFit, Render, RenderImage,
    SharedString, Styled, Window, div, img, size, uniform_list,
};

use crate::ui::contracts::{PreviewEvent, PreviewPayload, PreviewView};

/// 已开预览窗口注册表（entry_name → handle；进程内单例）。
static PREVIEW_WINDOWS: Mutex<Vec<(String, gpui_kit::WindowHandle<PreviewState>)>> =
    Mutex::new(Vec::new());

/// RGBA8 → gpui RenderImage（BGRA 预乘 swap，对齐 gpui svg_renderer 内部路径）。
///
/// 解码（PNG/JPEG/… → RGBA8）由调用方（T-G30）在后台线程完成；本函数只做
/// 格式转换——低开销，可进 UI 线程。
pub fn rgba_to_render_image(w: u32, h: u32, rgba: Vec<u8>) -> Arc<RenderImage> {
    let mut buffer = image::ImageBuffer::from_raw(w, h, rgba)
        .unwrap_or_else(|| panic!("rgba buffer 与 {w}x{h}x4 不匹配"));
    for pixel in buffer.chunks_exact_mut(4) {
        gpui_kit::swap_rgba_pa_to_bgra(pixel);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// 预乘 BGRA8 → gpui `RenderImage`（**不换通道**：源已是预乘 BGRA）。
///
/// 平台侧（`hzip_platform::file_icon`）产出的就是这种布局（T-ICON §4.1），
/// 与 gpui 后端期望的格式一致 ⇒ 系统图标位图直通封装，无需逐像素处理
/// （与 [`rgba_to_render_image`] 的 RGBA→BGRA swap 路径区分）。
///
/// 长度不符 → panic，语义与兄弟函数 [`rgba_to_render_image`] 相同；调用方
/// **必须先校验**（本仓库唯一调用点：`ui::widgets::file_icon` 的探测线程，
/// 校验不过按「拿不到图标」处理并 `tracing::warn!`）。
pub fn bgra_pa_to_render_image(w: u32, h: u32, bgra_pa: Vec<u8>) -> Arc<RenderImage> {
    let buffer = image::ImageBuffer::from_raw(w, h, bgra_pa)
        .unwrap_or_else(|| panic!("bgra buffer 与 {w}x{h}x4 不匹配"));
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// `PreviewData` → [`PreviewPayload`]（T-SEL4：补上 T-G30 未接线的派生层）。
///
/// 由确定模式分派：文本（core 已转码 UTF-8）按行切分；图片（PNG/JPEG/
/// GIF/BMP）解码 RGBA8——失败置 `image_rgba` None（渲染占位「无法解码」，
/// 不 panic）；HEX 按 16 字节/行（偏移 + hex + ASCII）。
///
/// 解码在后台线程调用（`open_selection_entry` 的 background spawn）；
/// 本函数只做派生与格式化，无 IO。
pub fn payload_from_preview(
    entry_name: &str,
    data: hzip_core::services::preview::PreviewData,
) -> PreviewPayload {
    use hzip_core::services::preview::PreviewMode;
    let (view, text, image, hex_lines) = match data.mode {
        PreviewMode::Text => {
            let s = String::from_utf8_lossy(&data.bytes).into_owned();
            (
                PreviewView::Text,
                std::sync::Arc::new(s.lines().map(str::to_string).collect()),
                None,
                std::sync::Arc::new(Vec::new()),
            )
        }
        PreviewMode::Image => {
            // 解码 + BGRA swap + `RenderImage` 封装**都在这里一次做完**（本函数
            // 在后台线程被调用，见 `open_selection_entry`）——旧实现把这一步留在
            // 渲染里，等于每帧重做一次大图解码/拷贝（审计 B16）。
            let rgba = image::load_from_memory(&data.bytes).ok().map(|img| {
                let (w, h) = (img.width(), img.height());
                rgba_to_render_image(w, h, img.into_rgba8().into_raw())
            });
            (
                PreviewView::Image,
                std::sync::Arc::new(Vec::new()),
                rgba,
                std::sync::Arc::new(Vec::new()),
            )
        }
        // Auto 已在 core 解析为确定模式（Hex 兜底）
        _ => {
            let lines: Vec<String> = data
                .bytes
                .chunks(16)
                .enumerate()
                .map(|(i, chunk)| {
                    let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
                    // 16 字节 hex 区固定宽 47（16×2 + 15 空格），不足补齐对齐
                    let ascii: String = chunk
                        .iter()
                        .map(|&b| {
                            if (0x20..0x7F).contains(&b) {
                                char::from(b)
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    format!("{:08X}  {:<47}  {}", i * 16, hex.join(" "), ascii)
                })
                .collect();
            (
                PreviewView::Hex,
                std::sync::Arc::new(Vec::new()),
                None,
                std::sync::Arc::new(lines),
            )
        }
    };
    PreviewPayload {
        entry_name: entry_name.to_string(),
        view,
        text,
        image,
        hex_lines,
        truncated: data.truncated,
        total_size: crate::ui::binding::fmt_bytes(data.total_size),
    }
}

/// 打开预览窗口（同 entry 重复打开 → 聚焦已有窗口）。
///
/// `on`：事件上抛（Switch 在窗口实体内部消费；OpenExternal/Close 上抛由
/// T-G30 接线）。
pub fn open_preview_window(
    cx: &mut App,
    payload: PreviewPayload,
    on: impl Fn(PreviewEvent) + 'static,
) -> AnyWindowHandle {
    let view = payload.view;
    let entry_name = payload.entry_name.clone();
    // 去重：已有同 entry 窗口且存活 → 更新其 payload 并聚焦
    {
        let Ok(mut windows) = PREVIEW_WINDOWS.lock() else {
            return open_fresh_window(cx, payload, view).into();
        };
        windows.retain(|(_, handle)| {
            handle
                .update(cx, |view, _window, cx| {
                    view.respawn(&payload);
                    cx.notify();
                })
                .is_ok()
        });
        if let Some((_, handle)) = windows
            .iter()
            .find(|(name, _)| *name == payload.entry_name)
            .cloned()
        {
            return handle.into();
        }
    }
    let handle = open_fresh_window(cx, payload, view);
    if let Ok(mut windows) = PREVIEW_WINDOWS.lock() {
        windows.push((entry_name, handle));
    }
    // T-GUI T33：回调注入窗口实体（Close/外部打开由窗口内按钮消费）
    let on = std::rc::Rc::new(on);
    let _ = handle.update(cx, |state, _window, cx| {
        state.on_event = Some(Box::new(move |e: PreviewEvent| on(e.clone())));
        cx.notify();
    });
    handle.into()
}

/// 直接开新窗口（去重锁不可用/未命中时的兜底路径）。
fn open_fresh_window(
    cx: &mut App,
    payload: PreviewPayload,
    view: PreviewView,
) -> gpui_kit::WindowHandle<PreviewState> {
    let entry_name = payload.entry_name.clone();
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
            gpui_kit::Bounds::centered(None, size(PREVIEW_WINDOW_W, PREVIEW_WINDOW_H), cx),
        )),
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(SharedString::from(crate::tr_with(
                "preview.title",
                &[("name", entry_name.as_str())],
            ))),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        ..Default::default()
    };
    cx.open_window(options, |_, cx| {
        cx.new(|_| PreviewState {
            payload,
            view,
            on_event: None,
        })
    })
    .unwrap_or_else(|e| panic!("预览窗口创建失败: {e}"))
}

/// 窗口内部状态（视图切换态在窗口实体内部，不发回主窗口）。
pub struct PreviewState {
    /// 载荷
    pub payload: PreviewPayload,
    /// 当前视图
    pub view: PreviewView,
    /// 事件上抛（T-GUI T33 接线：Close → 关窗；OpenExternal → 挂账）
    on_event: Option<Box<dyn Fn(PreviewEvent)>>,
}

impl PreviewState {
    /// 同 entry 重复打开：替换载荷并复位视图（聚焦语义由窗口层处理）。
    fn respawn(&mut self, payload: &PreviewPayload) {
        self.payload = payload.clone();
        self.view = payload.view;
    }

    /// 三视图分支渲染（每视图一 AnyElement；虚拟滚动行渲染）。
    fn view_element(&self, cx: &Context<Self>) -> AnyElement {
        match self.view {
            PreviewView::Text => self.text_list(cx),
            PreviewView::Image => self.image_view(cx),
            PreviewView::Hex => self.hex_list(cx),
        }
    }

    /// 文本虚拟滚动（行渲染；50k 行滚动流畅为验收基线——行数不影响 DOM 规模）。
    fn text_list(&self, _cx: &Context<Self>) -> AnyElement {
        // Arc 克隆（重绘零行拷贝；行文本按需在可见范围内生成）
        let lines = self.payload.text.clone();
        uniform_list("preview-text", lines.len(), move |range, _, _| {
            range
                .map(|ix| {
                    div()
                        .h(MONO_ROW_H)
                        .font_family("Consolas")
                        .text_sm()
                        .whitespace_nowrap()
                        .child(SharedString::from(lines[ix].clone()))
                })
                .collect()
        })
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    /// HEX 虚拟滚动（payload.hex_lines 已按 16 字节/行切好：偏移 + hex + ASCII）。
    fn hex_list(&self, _cx: &Context<Self>) -> AnyElement {
        let lines = self.payload.hex_lines.clone();
        uniform_list("preview-hex", lines.len(), move |range, _, _| {
            range
                .map(|ix| {
                    div()
                        .h(MONO_ROW_H)
                        .font_family("Consolas")
                        .text_xs()
                        .whitespace_nowrap()
                        .child(SharedString::from(lines[ix].clone()))
                })
                .collect()
        })
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    /// 图片视图（RGBA8 → ImageSource::Render；解码失败占位「无法解码」）。
    fn image_view(&self, cx: &Context<Self>) -> AnyElement {
        match &self.payload.image {
            Some(render_image) => {
                // 预封好的 `RenderImage`（载荷构造时在后台线程做过一次解码/swap）
                // ——这里只克隆 Arc，渲染帧零拷贝（审计 B16）。
                let render_image = render_image.clone();
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(render_image)
                            .object_fit(ObjectFit::Contain)
                            .max_w_full()
                            .max_h_full(),
                    )
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(crate::tr("preview.decode_failed")))
                .into_any_element(),
        }
    }
}

impl Render for PreviewState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let truncated = self.payload.truncated;
        let total = self.payload.total_size.clone();
        let view = self.view;

        let tab_btn = |id: &'static str, label_key: &'static str, target: PreviewView| {
            let selected = view == target;
            Button::new(id)
                .with_size(component_size())
                .label(crate::tr(label_key))
                .selected(selected)
                .small()
                .on_click(cx.listener(move |this: &mut Self, _, _, cx| {
                    this.view = target;
                    cx.notify();
                }))
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                // 顶行：三视图切换 + 外部打开
                h_flex()
                    .flex_shrink_0()
                    .justify_between()
                    .items_center()
                    .px(sp(2.))
                    .py(sp(1.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap(sp(1.))
                            .child(tab_btn("pv-text", "preview.tab_text", PreviewView::Text))
                            .child(tab_btn("pv-image", "preview.tab_image", PreviewView::Image)),
                    )
                    .child(
                        Button::new("pv-external")
                            .with_size(component_size())
                            .label(crate::tr("preview.open_external"))
                            .small()
                            .on_click(|_, _, _| {
                                // T-GUI T33：OpenExternal 需把档案内条目落
                                // 临时文件再系统打开（TempArea 写盘 + 源
                                // 路径解析归 R-GUI3 预览装配链）——当前
                                // payload 只有内存数据，系统打开无法路由，
                                // 保持 no-op 并登记挂账（原空闭包不动语义）。
                            }),
                    ),
            )
            .child(self.view_element(cx))
            .child(
                // 底行：截断提示 + 关闭
                h_flex()
                    .flex_shrink_0()
                    .justify_between()
                    .items_center()
                    .px(sp(2.))
                    .py(sp(1.))
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(if truncated {
                        div()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(SharedString::from(crate::tr_with(
                                "preview.truncated",
                                &[("total", total.as_str())],
                            )))
                    } else {
                        div()
                    })
                    .child(
                        Button::new("pv-close")
                            .with_size(component_size())
                            .label(crate::tr("preview.close"))
                            .small()
                            .on_click(|_, window, _| {
                                // T-GUI T33 接线：关闭按钮真正关窗
                                //（原按钮无回调——点击无反应）
                                window.remove_window();
                            }),
                    ),
            )
    }
}

#[cfg(test)]
#[path = "tests/preview.rs"]
mod tests;

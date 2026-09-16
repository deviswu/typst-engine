//! 实时 Typst 预览器 —— `typst-engine` 的 GPUI 外壳。
//!
//! 左边敲字，右边在**几毫秒内**跟着变。中间没有子进程、没有 IPC、
//! 没有存盘：编辑器把文本喂给引擎的内存覆盖层，引擎在同一个进程里重排版。
//!
//! ```bash
//! cargo run -p typst-live              # 用内置示例文档
//! cargo run -p typst-live -- doc.typ   # 打开指定文件
//! ```
//!
//! **排版与光栅化是两层**：缩放只重做光栅化，不重新排版。状态栏上
//! 「排版 N 次 / 光栅化 M 次」两个计数会把这件事直接显示出来 ——
//! 按 Ctrl+= 缩放时你只会看到后一个数在涨。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui::*;
use gpui_component::highlighter::{
    Diagnostic as Squiggle, DiagnosticSeverity, LanguageConfig, LanguageRegistry,
};
use gpui_component::input::{Input, InputEvent, InputState, Position};
use gpui_component::{ActiveTheme as _, Root, h_flex, v_flex};

use typst::diag::SourceDiagnostic;
use typst::foundations::Bytes;
use typst_engine::export::{RasterPage, pdf as export_pdf, pixel_per_pt_for_zoom, rasterize};
use typst_engine::syntax as lang;
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

actions!(
    typst_live,
    [
        ZoomIn,
        ZoomOut,
        ZoomReset,
        SaveFile,
        RecompileNow,
        FormatDocument,
        ExportPdf,
    ]
);

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
const ZOOM_STEP: f32 = 1.25;

const DEMO_DOC: &str = r#"#set page(width: 15cm, height: auto, margin: 1.8cm)
#set text(size: 11pt)

= 实时编译演示

在*左边随便改点什么*，右边会在几毫秒内跟着变。

没有子进程，没有存盘，没有 IPC —— 编译器直接读内存里未保存的文本。

== 试试缩放

按 *Ctrl+=* 放大、*Ctrl+-* 缩小、*Ctrl+0* 回到 100%。

看状态栏：*排版次数不会变，光栅化次数才会变* —— 因为缩放只是重新出图，
没有重新排版。DPI 完全由我们自己决定，所以放到 400% 也不会糊。

== 为什么能这么快

+ 覆盖式虚拟文件系统：编译器看到的就是你正在敲的内容
+ `Source::replace` 的增量重解析：敲一个字只重解析几十字节
+ comemo 记忆化排版：没变的部分不重算

== 数学与表格

$ integral_0^infinity e^(-x^2) dif x = sqrt(pi)/2 $

#table(
  columns: 3,
  [*指标*], [*冷编译*], [*敲键*],
  [耗时], [191 ms], [1.9 ms],
  [重解析], [全文 34 KB], [276 B],
  [加速], [—], [102×],
)

== 打错字也不会白屏

把下面这行的括号删掉试试 —— 右边会保留上一次编译成功的结果，
只在下方列出错误，而不是变成一片空白。

#let broken = (1 + 2)
"#;

fn register_typst_language() {
    LanguageRegistry::singleton().register(
        "typst",
        &LanguageConfig::new(
            "typst",
            codebook_tree_sitter_typst::LANGUAGE.into(),
            vec![],
            include_str!("../typst-highlights.scm"),
            "",
            "",
        ),
    );
}

#[derive(Default)]
struct Status {
    /// 排版耗时（`typst::compile`）。
    compile_ms: f64,
    /// 光栅化耗时（`typst_engine::export::rasterize`）。
    raster_ms: f64,
    reparsed: usize,
    text_bytes: usize,
    pages: usize,
    /// 排版次数。缩放**不会**让它增加。
    compiles: usize,
    /// 光栅化次数。缩放**会**让它增加。
    rasters: usize,
}

struct Previewer {
    engine: EngineWorld,
    main_path: PathBuf,
    editor: Entity<InputState>,
    /// 排版结果。缩放时原样不动 —— 这是「排版 / 光栅化」两层的分界。
    doc: Option<Arc<PagedDocument>>,
    /// 缩放倍率。1.0 = 屏幕上「实际大小」（96 dpi）。
    zoom: f32,
    /// 窗口所在显示器的缩放系数（Windows 的「缩放与布局」，常见 1.0 / 1.25 / 1.5）。
    ///
    /// **必须参与光栅化**：`zoom` 说的是「文档要占多大的逻辑尺寸」，
    /// 而真正要填满的是**物理像素**。1 逻辑像素 = `scale_factor` 个物理像素，
    /// 所以按 96 dpi 出图、再让 gpui 按 1.5 放大，结果就是糊的。
    /// （初版就踩了这个坑：把「1 纹理像素 = 1 逻辑像素」当成了
    /// 「1 纹理像素 = 1 物理像素」，于是在 150% 缩放的屏上字发虚。）
    scale_factor: f32,
    /// 文档大纲。来自 `typst-syntax`，不是正则。
    outline: Vec<lang::OutlineItem>,
    /// 本次排版产出的原始诊断。留着是因为要把它们**映射成字节范围**
    /// 去画波浪线，而转成字符串就找不回来了。
    compile_errors: Vec<SourceDiagnostic>,
    /// 语法错误 + 编译错误里「错误」的条数。
    error_count: usize,
    /// 文本规模（字/词/行）。
    stats: lang::TextStats,
    /// 自上次保存以来改过没有。
    dirty: bool,
    /// 一次性的操作反馈（已保存 / 已格式化 / 导出到哪）。
    message: Option<String>,
    /// 光栅化产物（GPU 纹理）。每次排版或缩放后重建。
    bitmaps: Vec<Arc<RenderImage>>,
    /// 当前纹理共占多少字节。用来把显存开销直接显示给用户。
    texture_bytes: usize,
    /// 上一次已排版的文本。
    ///
    /// gpui-component 的 `InputEvent::Change` 不止在文字变化时发出
    /// （光标移动、选中也会），不挡一下就会白排一遍。
    last_text: String,
    status: Status,
    _sub: Subscription,
}

impl Previewer {
    fn new(
        source: String,
        main_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = main_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut engine = EngineWorld::new(
            embedded_and_system_fonts(),
            EntryState::new(root, &main_path),
        );
        let main_id = engine.entry().main();

        // 把初始文本放进内存覆盖层 + 语法树，这样磁盘上有没有这个文件都无所谓。
        engine
            .vfs_mut()
            .map_shadow(&main_path, Bytes::from_string(source.clone()));
        engine.sources().feed_memory(main_id, &source);

        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .code_editor("typst")
                .default_value(source)
        });

        let sub = cx.subscribe_in(&editor, window, |this, _, event, _window, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_editor_change(cx);
            }
        });

        let mut this = Self {
            engine,
            main_path,
            editor,
            doc: None,
            zoom: 1.0,
            scale_factor: window.scale_factor(),
            outline: Vec::new(),
            compile_errors: Vec::new(),
            error_count: 0,
            stats: lang::TextStats::default(),
            dirty: false,
            message: None,
            bitmaps: Vec::new(),
            texture_bytes: 0,
            last_text: String::new(),
            status: Status::default(),
            _sub: sub,
        };
        // 启动就把焦点给编辑器：否则用户打不了字，
        // 而且 gpui 的动作派发**从聚焦节点开始**，没有焦点时
        // Ctrl+S 之类的全局快捷键根本不会触达 on_action。
        this.editor.update(cx, |state, cx| state.focus(window, cx));

        // 首次编译不走守卫：文本为空也该先把世界建起来。
        this.recompile(cx);
        this
    }

    /// 编辑器内容变化。**先挡一道**：内容没真的变就不排版。
    fn on_editor_change(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        if text == self.last_text {
            return;
        }
        self.dirty = true;
        self.recompile(cx);
    }

    /// 一次完整的「编辑 → 重排版 → 重新出图」循环。
    ///
    /// 同步执行：排版只要几毫秒，开线程反而引入调度开销与状态同步的麻烦。
    fn recompile(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        self.last_text = text.clone();
        let main_id = self.engine.entry().main();

        // ① 未保存文本进覆盖层
        self.engine
            .vfs_mut()
            .map_shadow(&self.main_path, Bytes::from_string(text.clone()));
        // ② 增量重解析（不是重建语法树）
        let fed = self.engine.sources().feed_memory(main_id, &text);
        self.status.reparsed = fed.reparsed.map(|r| r.len()).unwrap_or(0);
        self.status.text_bytes = text.len();

        // ③ 排版。「上一次成功的结果」由**引擎**记着 —— 失败时 `compiled.doc`
        //    就是那份旧的，外壳不需要再自己维护一份副本。
        let compiled = self.engine.compile();
        self.status.compile_ms = compiled.elapsed.as_secs_f64() * 1000.0;
        self.status.compiles = self.engine.compile_attempts();

        match compiled.doc {
            Some(doc) => {
                self.status.pages = doc.pages().len();
                // 只有文档真的换了才重新出图。失败时拿到的是同一个 Arc，
                // 重新光栅化纯属浪费。
                let changed = !self.doc.as_ref().is_some_and(|old| Arc::ptr_eq(old, &doc));
                self.doc = Some(doc);
                if changed {
                    self.rerasterize();
                }
            }
            None => {
                self.doc = None;
                self.bitmaps.clear();
                self.texture_bytes = 0;
            }
        }

        if compiled.fresh {
            self.compile_errors.clear();
        } else {
            self.compile_errors = compiled.errors;
        }

        // ④ 语法服务。刻意用**引擎里那棵增量维护的树**
        //    （`feed_memory` 刚就地重解析过），而不是 `Source::detached(text)`
        //    再全量 parse 一遍。
        self.refresh_language_services(cx);

        cx.notify();
    }

    /// 大纲 + 波浪线。两者都基于同一个 `Source`，所以一起算。
    fn refresh_language_services(&mut self, cx: &mut Context<Self>) {
        let Ok(source) = typst::World::source(&self.engine, self.engine.entry().main()) else {
            return;
        };

        self.outline = lang::outline(&source);
        self.stats = lang::text_stats(source.text());

        // 语法错误（不编译就有）与编译错误**形状相同**，所以
        // 交给同一个渲染器，不需要区分来源。
        let mut diags = lang::syntax_diagnostics(&source);
        diags.extend(lang::compile_diagnostics(&source, &self.compile_errors));
        self.error_count = diags.iter().filter(|d| d.is_error).count();

        let rope = self.editor.read(cx).text().clone();
        self.editor.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&rope);
                for d in &diags {
                    let Some(start) = d.line_col else { continue };
                    let end = squiggle_end(&source, d, start);
                    let severity = if d.is_error {
                        DiagnosticSeverity::Error
                    } else {
                        DiagnosticSeverity::Warning
                    };
                    set.push(
                        Squiggle::new(
                            Position::new(start.line as u32, start.col as u32)
                                ..Position::new(end.line as u32, end.col as u32),
                            d.message.clone(),
                        )
                        .with_severity(severity),
                    );
                }
            }
            cx.notify();
        });
    }

    /// 从已排版的 `doc` 重新出图。**不碰编译器**。
    fn rerasterize(&mut self) {
        let Some(doc) = self.doc.clone() else {
            self.bitmaps.clear();
            return;
        };

        let ppp = pixel_per_pt_for_zoom(self.zoom) * self.scale_factor;
        let t = Instant::now();
        let pages = rasterize(&doc, ppp);
        self.status.raster_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.status.rasters += 1;

        let total: usize = pages.iter().map(|p| p.rgba.len()).sum();
        self.texture_bytes = total;
        self.bitmaps = pages.into_iter().filter_map(to_texture).collect();

        println!(
            "[typst-live] 光栅化 {} 页 @ {:.0}% × 屏缩放 {:.2}（{:.2} px/pt）：{:.1} ms，纹理共 {:.1} MiB",
            self.bitmaps.len(),
            self.zoom * 100.0,
            self.scale_factor,
            ppp,
            self.status.raster_ms,
            total as f64 / (1024.0 * 1024.0),
        );
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = zoom;
        // 只重做出图，不重新排版 —— 这就是缩放能任意清晰的原因。
        self.rerasterize();
        cx.notify();
    }

    /// Ctrl+S：把编辑器里的文本写到 `main_path`。
    fn save_file(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();

        match std::fs::write(&self.main_path, &text) {
            Ok(()) => {
                // 磁盘与内存一致了，撤掉覆盖层 —— 从此磁盘就是真相。
                // （主文件的文本还在 SourceDb 里，所以不会因此重新解析。）
                self.engine.vfs_mut().unmap_shadow(&self.main_path);
                self.dirty = false;
                self.message = Some(format!("已保存 {}", self.main_path.display()));
            }
            Err(err) => self.message = Some(format!("保存失败：{err}")),
        }
        cx.notify();
    }

    /// Ctrl+B：手动重新编译。
    ///
    /// 不只是「再排一次」—— 它先把源文件缓存**全部作废**，
    /// 这样被 `#include` 的文件如果被外部改过也能重新读进来
    /// （我们没有文件监听，所以这是手动补上那个缺口）。
    /// 主文件的未保存编辑不会丢：它的文本同时存在于 VFS 覆盖层里。
    fn recompile_now(&mut self, cx: &mut Context<Self>) {
        self.engine.sources().invalidate_all();
        self.message = Some("已重新编译（源文件缓存已作废）".to_owned());
        self.recompile(cx);
    }

    /// Ctrl+Shift+F：进程内格式化。
    fn format_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();

        match typst_engine::format::format(&text) {
            Ok(formatted) if formatted == text => {
                self.message = Some("已经是格式化状态".to_owned());
            }
            Ok(formatted) => {
                // `set_value` 刻意不发 Change 事件（`emit_events = false`），
                // 所以得自己触发一次重排。
                self.editor
                    .update(cx, |state, cx| state.set_value(formatted, window, cx));
                self.dirty = true;
                self.message = Some("已格式化".to_owned());
                self.recompile(cx);
            }
            Err(err) => {
                self.message = Some(format!("格式化失败（语法可能不完整）：{err}"));
            }
        }
        cx.notify();
    }

    /// Ctrl+E：导出 PDF。
    ///
    /// 手上已经有 `PagedDocument`，所以不重新排版、不重读文件、不起子进程。
    fn export_pdf(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.clone() else {
            self.message = Some("还没有可导出的排版结果".to_owned());
            cx.notify();
            return;
        };

        let out = self.main_path.with_extension("pdf");
        self.message = Some(match export_pdf(&doc) {
            Ok(bytes) => match std::fs::write(&out, bytes) {
                Ok(()) => format!("已导出 {}", out.display()),
                Err(err) => format!("写 PDF 失败：{err}"),
            },
            Err(errors) => format!("导出失败：{} 条诊断", errors.len()),
        });
        cx.notify();
    }

    /// 大纲点击：把光标移到那一行。
    fn jump_to_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.set_cursor_position(Position::new(line as u32, 0), window, cx);
        });
        self.message = Some(format!("已跳到第 {} 行", line + 1));
        cx.notify();
    }

    fn render_statusbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let bar = h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(
                div()
                    .text_color(theme.primary)
                    .child(format!("排版 {:.1} ms", self.status.compile_ms)),
            )
            .child(format!("光栅化 {:.1} ms", self.status.raster_ms))
            .child(format!(
                "重解析 {} B / {} B",
                self.status.reparsed, self.status.text_bytes
            ))
            .child(format!(
                "{:.0} MiB",
                self.texture_bytes as f64 / (1024.0 * 1024.0)
            ))
            .child(format!("{} 页", self.status.pages))
            .child(
                div()
                    .text_color(theme.primary)
                    .child(format!("缩放 {:.0}%", self.zoom * 100.0)),
            )
            .child(format!(
                "排版 {} 次 / 光栅化 {} 次",
                self.status.compiles, self.status.rasters
            ));

        // 第二行：文档信息 + 保存状态 + 一次性反馈。
        // 拆两行是因为第一行已被引擎指标占满，再塞就只看得花。
        let mut info = h_flex()
            .w_full()
            .px_3()
            .py_1()
            .gap_4()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(self.stats.summary())
            .child(format!("{} 个标题", self.outline.len()))
            .child(if self.dirty {
                "● 未保存"
            } else {
                "已保存"
            });

        if self.error_count > 0 {
            info = info.child(div().text_color(theme.danger).child(format!(
                "✗ {} 个错误（预览保留上次成功结果）",
                self.error_count
            )));
        } else {
            info = info.child(div().text_color(theme.success).child("✓ 无错误"));
        }

        if let Some(msg) = &self.message {
            info = info.child(div().text_color(theme.primary).child(msg.clone()));
        }

        v_flex().w_full().child(bar).child(info)
    }

    /// 左侧大纲栏。
    fn render_outline(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let body = v_flex()
            .id("outline-body")
            .flex_1()
            .overflow_y_scroll()
            .py_1()
            .children(if self.outline.is_empty() {
                vec![
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("（还没有标题）")
                        .into_any_element(),
                ]
            } else {
                // 先收成自有数据再建元素：避免在闭包里借用 `self.outline`。
                let rows: Vec<(usize, String, usize)> = self
                    .outline
                    .iter()
                    .map(|i| (i.depth, i.title.clone(), i.line))
                    .collect();

                rows.into_iter()
                    .map(|(depth, title, line)| {
                        div()
                            .px_3()
                            .py_0p5()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(if depth == 1 {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .hover(|s| s.bg(theme.accent.opacity(0.12)))
                            .child(format!(
                                "{}{}",
                                "    ".repeat(depth.saturating_sub(1)),
                                title
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                    this.jump_to_line(line, window, cx);
                                }),
                            )
                            .into_any_element()
                    })
                    .collect()
            });

        v_flex()
            .w(px(228.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .border_b_1()
                    .border_color(theme.border)
                    .child("大纲"),
            )
            .child(body)
    }

    fn render_errors(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_1()
            .bg(theme.danger.opacity(0.08))
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.danger)
            .children(
                self.compile_errors
                    .iter()
                    .take(6)
                    .map(|e| div().child(e.message.to_string())),
            )
    }
}

/// 波浪线的终点。
///
/// 至少覆盖一个字符；若诊断带着可用的字节范围、且终点**在同一行**，
/// 就盖住整个病灶而不是戳一个孤零零的点（跨行的话用一个字符收尾，
/// 免得算出一段横跨多行的矩形）。
fn squiggle_end(
    source: &typst::syntax::Source,
    d: &lang::Diagnostic,
    start: lang::LineCol,
) -> lang::LineCol {
    let one_char = lang::LineCol {
        line: start.line,
        col: start.col + 1,
    };

    let Some(range) = d.range.as_ref() else {
        return one_char;
    };
    let end = lang::line_col(source, range.end);

    if end.line == start.line && end.col > start.col {
        end
    } else {
        one_char
    }
}

/// 引擎的 `RasterPage` → GPUI 的 GPU 纹理。
///
/// 这里**同步**构造 `RenderImage`，刻意不走 `Image::from_bytes` 那条路 ——
/// 后者要过 asset 系统做异步解码（`use_asset`），第一次必然拿不到结果。
/// 而且 `Image::from_bytes` 的缓存键是内容哈希，同一份内容永远复用同一张
/// 纹理，缩放就拿不到更高分辨率。自己构造就没有这两个问题。
fn to_texture(page: RasterPage) -> Option<Arc<RenderImage>> {
    let (width, height) = (page.width, page.height);
    let mut rgba = page.rgba;

    // tiny-skia 给的是 RGBA（alpha 已预乘），GPU 纹理要 BGRA。
    for pixel in rgba.as_chunks_mut::<4>().0 {
        swap_rgba_pa_to_bgra(pixel);
    }

    let buffer: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_raw(width, height, rgba)?;

    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

impl Render for Previewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 窗口可能被拖到另一块缩放不同的显示器上。
        // 系数变了就得按新的重新出图，否则要么糊（变大）、要么白费显存（变小）。
        let scale_factor = window.scale_factor();
        if (scale_factor - self.scale_factor).abs() > f32::EPSILON {
            println!(
                "[typst-live] 显示器缩放变了：{:.2} → {:.2}，重新出图",
                self.scale_factor, scale_factor
            );
            self.scale_factor = scale_factor;
            self.rerasterize();
        }

        let theme = cx.theme();
        let has_errors = !self.compile_errors.is_empty();

        let outline_pane = self.render_outline(cx);

        let editor_pane = div()
            .w(px(520.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .child(Input::new(&self.editor).h_full());

        // 纹理已是最终分辨率，按 1:1 显示即可 —— 缩放倍率已经体现在像素数里。
        let pages = v_flex()
            .id("pages")
            .flex_1()
            .h_full()
            .overflow_scroll()
            .bg(theme.secondary)
            .gap_5()
            .py_5()
            .items_center()
            .children(self.bitmaps.iter().map(|bitmap| {
                let size = bitmap.size(0);
                // 纹理是**物理像素**，而布局用的是**逻辑像素** —— 除回缩放系数
                // 就是 1:1 映射，一个物理像素对一个物理像素，字才不发虚。
                let logical_w = size.width.0 as f32 / self.scale_factor;
                let logical_h = size.height.0 as f32 / self.scale_factor;
                div().bg(gpui::white()).shadow_md().overflow_hidden().child(
                    img(ImageSource::Render(bitmap.clone()))
                        .w(px(logical_w))
                        .h(px(logical_h)),
                )
            }));

        let preview_pane = if has_errors {
            v_flex()
                .flex_1()
                .h_full()
                .child(pages)
                .child(self.render_errors(cx))
                .into_any_element()
        } else {
            pages.into_any_element()
        };

        v_flex()
            .size_full()
            .bg(theme.background)
            .child(self.render_statusbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(outline_pane)
                    .child(editor_pane)
                    .child(preview_pane),
            )
            .on_action(cx.listener(|this, _: &ZoomIn, _window, cx| {
                let next = this.zoom * ZOOM_STEP;
                this.set_zoom(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _window, cx| {
                let next = this.zoom / ZOOM_STEP;
                this.set_zoom(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomReset, _window, cx| {
                this.set_zoom(1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &SaveFile, _window, cx| {
                this.save_file(cx);
            }))
            .on_action(cx.listener(|this, _: &RecompileNow, _window, cx| {
                this.recompile_now(cx);
            }))
            .on_action(cx.listener(|this, _: &FormatDocument, window, cx| {
                this.format_document(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExportPdf, _window, cx| {
                this.export_pdf(cx);
            }))
    }
}

/// 载入要编辑的文档：有参数就用它，否则用内置示例。
fn load_document(arg: Option<String>) -> (String, PathBuf) {
    if let Some(raw) = arg {
        let path = PathBuf::from(&raw);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                println!("打开 {}", path.display());
                return (text, path);
            }
            Err(err) => {
                eprintln!("读不了 {raw}：{err}；改用内置示例文档");
            }
        }
    }

    // 示例文档落在临时目录。注意：磁盘上存不存在都无所谓 ——
    // 引擎用的是内存覆盖层，这正是实时编译的前提。
    let path = std::env::temp_dir().join("typst-live-demo.typ");
    println!("使用内置示例文档（虚拟路径 {}）", path.display());
    (DEMO_DOC.to_owned(), path)
}

fn main() {
    let path_arg = std::env::args().nth(1);

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        // 用 gpui-component 的任何东西之前必须先 init。
        gpui_component::init(cx);
        register_typst_language();

        cx.bind_keys([
            KeyBinding::new("ctrl-=", ZoomIn, None),
            KeyBinding::new("ctrl-+", ZoomIn, None),
            KeyBinding::new("ctrl--", ZoomOut, None),
            KeyBinding::new("ctrl-0", ZoomReset, None),
            KeyBinding::new("ctrl-s", SaveFile, None),
            KeyBinding::new("ctrl-b", RecompileNow, None),
            KeyBinding::new("ctrl-shift-f", FormatDocument, None),
            KeyBinding::new("ctrl-e", ExportPdf, None),
        ]);

        let (source, path) = load_document(path_arg);

        cx.spawn(async move |cx| {
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("typst-engine · 实时预览".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(80.), px(60.)),
                    Size {
                        width: px(1320.),
                        height: px(880.),
                    },
                ))),
                window_min_size: Some(Size {
                    width: px(900.),
                    height: px(600.),
                }),
                ..Default::default()
            };

            let source = source.clone();
            let path = path.clone();
            cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| Previewer::new(source.clone(), path.clone(), window, cx));
                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            })
            .expect("打开窗口失败");
        })
        .detach();
    });
}

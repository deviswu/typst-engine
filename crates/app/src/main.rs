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
use typst_engine::export::{RasterPage, pdf as export_pdf, pixel_per_pt_for_zoom, rasterize_page};
use typst_engine::syntax as lang;
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

mod finder;

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
        PrevPage,
        NextPage,
        FirstPage,
        LastPage,
        QuickOpen,
        QuickPrev,
        QuickNext,
        QuickCancel,
    ]
);

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
const ZOOM_STEP: f32 = 1.25;

/// 扫描候选文件的上限。再多就不适合靠打字找了。
const MAX_SCAN_FILES: usize = 5000;

/// 快速打开列表里最多显示多少条。
const MAX_QUICK_MATCHES: usize = 50;

/// 可见范围外再预出几页。留 1 页是为了滚动时不会先看到空白。
const PAGE_PREFETCH: usize = 1;

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

    /// 项目根目录。快速打开从这里往下找文件。
    root: PathBuf,
    /// 候选文件（相对 root）。每次开快速打开时重扫。
    files: Vec<PathBuf>,
    quick_input: Entity<InputState>,
    /// 当前匹配结果（已排序）。
    quick_matches: Vec<PathBuf>,
    quick_selected: usize,
    quick_visible: bool,
    _quick_sub: Subscription,
    /// 每页的光栅化纹理。`None` = 还没出图（或已被视口卸载）。
    ///
    /// **不一次性光栅化全部页**：A4 单页在 100% 下就 7.7 MiB，
    /// 100 页就是 770 MiB；400% 时每页 54 MiB。现在只留可见的几页。
    bitmaps: Vec<Option<Arc<RenderImage>>>,
    /// 每页的**逻辑**尺寸（宽, 高）。布局靠它，不靠纹理 ——
    /// 所以某一页没出图时占位尺寸依旧正确，滚动位置与翻页都不受影响。
    page_sizes: Vec<(f32, f32)>,
    /// 当前已经出图的页范围（含），用于判断「要不要重新同步」。
    live_range: Option<(usize, usize)>,
    /// 预览滚动区的句柄。翻页就是把某一页滚到顶部。
    scroll: ScrollHandle,
    /// 当前页（0 起）。翻页与缩放后回锚都靠它。
    current_page: usize,
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

        // 快速打开的查询框。做成常驻的（不是每次开关都新建）——
        // 开关只是切一个可见性标志，不涉及 entity 与订阅的生死。
        let quick_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("文件名或路径片段 · ↑↓ 选择 · Enter 打开 · Esc 取消")
                .submit_on_enter(true)
        });
        let quick_sub =
            cx.subscribe_in(
                &quick_input,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::Change => this.refresh_quick_matches(cx),
                    InputEvent::PressEnter { .. } => this.confirm_quick(window, cx),
                    _ => {}
                },
            );

        let root = main_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

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
            root,
            files: Vec::new(),
            quick_input,
            quick_matches: Vec::new(),
            quick_selected: 0,
            quick_visible: false,
            _quick_sub: quick_sub,
            bitmaps: Vec::new(),
            page_sizes: Vec::new(),
            live_range: None,
            scroll: ScrollHandle::new(),
            current_page: 0,
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
    /// 排版或缩放变了 —— 重建页尺寸并丢掉全部纹理，等下一帧按视口重新出图。
    ///
    /// **不再一次性光栅化全部页**。原因：A4 单页在 100% 下就占 7.7 MiB 纹理，
    /// 100 页就是 770 MiB；400% 时每页 54 MiB，直接爆。现在只出可见的几页。
    fn rerasterize(&mut self) {
        let Some(doc) = self.doc.clone() else {
            self.bitmaps.clear();
            self.page_sizes.clear();
            self.texture_bytes = 0;
            self.live_range = None;
            return;
        };

        // 页的**逻辑**尺寸（布局用）。它等于 pt × 96/72 × zoom ——
        // 屏幕缩放系数在这里恰好抵消，因为逻辑尺寸本就不该依赖显示器。
        let base = typst_engine::export::BASE_PIXEL_PER_PT * self.zoom;
        self.page_sizes = doc
            .pages()
            .iter()
            .map(|p| {
                let s = p.frame.size();
                (s.x.to_pt() as f32 * base, s.y.to_pt() as f32 * base)
            })
            .collect();

        // 全部丢成「未光栅化」占位。布局靠 page_sizes，不靠纹理，
        // 所以占位不会让页面尺寸变化 —— 滚到底、翻页、跳大纲都不受影响。
        self.bitmaps = vec![None; self.page_sizes.len()];
        self.texture_bytes = 0;
        self.live_range = None;
    }

    /// 按当前视口把该出图的页出了，把不该留的丢掉。
    ///
    /// 只在**可见页范围真的变了**的时候动手 —— 滚动过程中每帧都重算
    /// 会让滚动发涩。
    fn sync_visible_pages(&mut self) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let count = doc.pages().len();
        if count == 0 || self.bitmaps.len() != count {
            return;
        }

        // `top_item`/`bottom_item` 是 gpui 给的「当前滚进视口的子项下标」——
        // 我们的子项恰好就是页。还没布局时它们安全地返回 0。
        let first = self.scroll.top_item().min(count - 1);
        let last = self.scroll.bottom_item().min(count - 1);
        let lo = first.saturating_sub(PAGE_PREFETCH);
        let hi = (last + PAGE_PREFETCH).min(count - 1);

        self.current_page = first;
        if self.live_range == Some((lo, hi)) {
            return;
        }
        self.live_range = Some((lo, hi));

        let ppp = pixel_per_pt_for_zoom(self.zoom) * self.scale_factor;
        let t = Instant::now();
        let mut rasterized = 0usize;
        let mut dropped = 0usize;

        // ① 卸载：范围外的纹理丢掉（这是显存真正被释放的地方）
        for (i, slot) in self.bitmaps.iter_mut().enumerate() {
            if (i < lo || i > hi) && slot.is_some() {
                *slot = None;
                dropped += 1;
            }
        }

        // ② 补缺：范围内还没出图的页
        for i in lo..=hi {
            if self.bitmaps[i].is_some() {
                continue;
            }
            let texture = to_texture(rasterize_page(&doc.pages()[i], ppp));
            self.bitmaps[i] = texture;
            rasterized += 1;
        }

        if rasterized > 0 || dropped > 0 {
            self.status.raster_ms = t.elapsed().as_secs_f64() * 1000.0;
            self.status.rasters += 1;
        }
        self.texture_bytes = self
            .bitmaps
            .iter()
            .flatten()
            .map(|b| {
                let s = b.size(0);
                s.width.0 as usize * s.height.0 as usize * 4
            })
            .sum();

        let live = self.bitmaps.iter().filter(|b| b.is_some()).count();
        println!(
            "[typst-live] 视口 {}–{} 页 / 共 {count}：新出图 {rasterized}，卸载 {dropped}；\
             常驻纹理 {live} 页 {:.1} MiB，用时 {:.1} ms",
            lo + 1,
            hi + 1,
            self.texture_bytes as f64 / (1024.0 * 1024.0),
            self.status.raster_ms,
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

    /// 缩放并把当前页重新锚回预览顶部。
    fn set_zoom_anchored(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let before = self.zoom;
        self.set_zoom(zoom, cx);
        if (self.zoom - before).abs() > f32::EPSILON {
            self.scroll.scroll_to_top_of_item(self.current_page);
        }
    }

    /// 把第 `page` 页（0 起）滚到预览顶部。越界会被夹到合法范围。
    fn go_to_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.bitmaps.is_empty() {
            return;
        }
        let page = page.min(self.bitmaps.len() - 1);
        self.current_page = page;
        self.scroll.scroll_to_top_of_item(page);
        self.message = Some(format!("第 {} / {} 页", page + 1, self.bitmaps.len()));
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

    // ── 快速跳转（Ctrl+P）────────────────────────────────────────────

    /// Ctrl+P：扫描项目目录并弹出快速打开。
    fn show_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 每次重扫：文件是会被外部增删的，缓一份可能会看不到新建的文件。
        self.files = finder::scan(&self.root, MAX_SCAN_FILES);
        self.quick_visible = true;
        self.message = None;

        self.quick_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        // 空 query → 列全部（已排序），所以这里不用再调 refresh
        self.quick_matches = self.files.iter().take(MAX_QUICK_MATCHES).cloned().collect();
        self.quick_selected = 0;
        cx.notify();
    }

    /// 查询变化 → 重新排序。
    fn refresh_quick_matches(&mut self, cx: &mut Context<Self>) {
        if !self.quick_visible {
            return;
        }
        let query = self.quick_input.read(cx).value().trim().to_string();

        self.quick_matches = if query.is_empty() {
            self.files.iter().take(MAX_QUICK_MATCHES).cloned().collect()
        } else {
            finder::rank(&query, &self.files, MAX_QUICK_MATCHES)
        };
        self.quick_selected = 0;
        cx.notify();
    }

    /// ↑↓：在候选里移动。到头就停住，不循环 —— 循环会让人分不清首尾。
    fn quick_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.quick_matches.is_empty() {
            return;
        }
        let last = self.quick_matches.len() - 1;
        let next = self.quick_selected as isize + delta;
        self.quick_selected = next.clamp(0, last as isize) as usize;
        cx.notify();
    }

    /// Enter：打开选中的文件。
    fn confirm_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rel) = self.quick_matches.get(self.quick_selected).cloned() else {
            self.cancel_quick(cx);
            return;
        };
        let full = self.root.join(&rel);

        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(err) => {
                self.message = Some(format!("读不了 {}：{err}", full.display()));
                cx.notify();
                return;
            }
        };

        if full == self.main_path {
            self.cancel_quick(cx);
            return;
        }

        // 换文档：字体留着，入口 / 覆盖层 / 源缓存 / 上次成功结果都重置。
        self.engine.reopen(&self.root, &full);
        self.main_path = full;
        self.doc = None;
        self.bitmaps.clear();
        self.texture_bytes = 0;
        self.current_page = 0;
        self.dirty = false;
        // 置空是为了让下面的 recompile 不受「文本没变就不排」的干扰
        self.last_text = String::new();

        self.quick_visible = false;
        let label = rel.to_string_lossy().replace('\\', "/");
        self.message = Some(format!("打开 {label}"));

        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.focus(window, cx);
        });
        self.recompile(cx);
    }

    /// Esc / 点空白：关掉，不动已打开的文件。
    fn cancel_quick(&mut self, cx: &mut Context<Self>) {
        self.quick_visible = false;
        self.quick_matches.clear();
        self.quick_selected = 0;
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
            .child(if self.status.pages == 0 {
                "0 页".to_string()
            } else {
                // 跟着滚动走 —— 滚轮翻页时这个数字会跟着变
                format!("第 {} / {} 页", self.current_page + 1, self.status.pages)
            })
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

/// 快速打开的浮层。
impl Previewer {
    fn render_quick_open(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let rows: Vec<(String, usize)> = self
            .quick_matches
            .iter()
            .enumerate()
            .map(|(i, p)| (p.to_string_lossy().replace('\\', "/"), i))
            .collect();
        let total = self.quick_matches.len();

        let list = if rows.is_empty() {
            div()
                .px_3()
                .py_5()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.files.is_empty() {
                    "目录里没找到文件"
                } else {
                    "没有匹配的文件"
                })
                .into_any_element()
        } else {
            v_flex()
                .id("quick-list")
                .max_h(px(420.))
                .overflow_y_scroll()
                .children(rows.into_iter().map(|(label, i)| {
                    let selected = i == self.quick_selected;
                    let mut row = div().px_3().py_1().text_sm().cursor_pointer();
                    if selected {
                        row = row.bg(theme.accent.opacity(0.22));
                    }
                    row.hover(|s| s.bg(theme.accent.opacity(0.12)))
                        .child(label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                this.quick_selected = i;
                                this.confirm_quick(window, cx);
                            }),
                        )
                }))
                .into_any_element()
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(90.))
            .bg(gpui::black().opacity(0.25))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| this.cancel_quick(cx)),
            )
            .child(
                v_flex()
                    // 让 ↑↓ / Esc 的绑定只在这个子树里生效 ——
                    // 否则会跟编辑器自己的方向键打架。
                    .key_context("QuickOpen")
                    .w(px(640.))
                    .max_h(px(540.))
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_md()
                    .shadow_lg()
                    .overflow_hidden()
                    // 浮层内部的点击不该冒泡到外层的「点空白关闭」
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(Input::new(&self.quick_input).w_full())
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .border_t_1()
                            .border_color(theme.border)
                            .child(format!("{} / {} 个文件匹配", total, self.files.len())),
                    )
                    .child(list),
            )
    }
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

        // 按当前视口补出/卸载纹理。内部只在可见范围真的变了才动手，
        // 所以滚动过程中每帧调它是安全的（就是两次二分查找）。
        self.sync_visible_pages();

        let outline_pane = self.render_outline(cx);

        let editor_pane = div()
            .w(px(520.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .child(Input::new(&self.editor).h_full());

        // 布局尺寸一律用 `page_sizes`（按 pt 算出来的），**不用纹理的像素尺寸**。
        // 两者能对上（纹理 = pt × ppp，逻辑 = 纹理 / 屏缩放 = pt × 96/72 × zoom），
        // 但用前者才能保证「某一页还没出图」时页面尺寸不变 —— 否则
        // 滚动条会在滚到新页的瞬间跳一下。
        let page_rows: Vec<(f32, f32, Option<Arc<RenderImage>>)> = (0..self.bitmaps.len())
            .map(|i| {
                let (w, h) = self.page_sizes.get(i).copied().unwrap_or((0.0, 0.0));
                (w, h, self.bitmaps[i].clone())
            })
            .collect();

        let pages = v_flex()
            .id("pages")
            .flex_1()
            .h_full()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .bg(theme.secondary)
            .gap_5()
            .py_5()
            .items_center()
            .children(
                page_rows
                    .into_iter()
                    .enumerate()
                    .map(|(i, (w, h, texture))| {
                        let inner = match texture {
                            // 纹理是**物理像素**，而布局用的是**逻辑像素** —— 除回缩放系数
                            // 就是 1:1 映射，一个物理像素对一个物理像素，字才不发虚。
                            Some(texture) => img(ImageSource::Render(texture))
                                .w(px(w))
                                .h(px(h))
                                .into_any_element(),
                            // 占位：尺寸一样，所以不牵动布局；只是还没出图
                            None => div()
                                .w(px(w))
                                .h(px(h))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(gpui::black().opacity(0.18))
                                .child(format!("第 {} 页", i + 1))
                                .into_any_element(),
                        };
                        div()
                            .bg(gpui::white())
                            .shadow_md()
                            .overflow_hidden()
                            .w(px(w))
                            .h(px(h))
                            // ★ 必须禁掉收缩：flex 列容器默认 `flex-shrink: 1`，
                            // 会把超出视口高度的页全压扁塞进来 —— 结果是
                            // **容器永远不会溢出，滚轮因此没有任何东西可滚**，
                            // 而且视口光栅化会误以为「全部页都可见」而把纹理全出出来。
                            .flex_shrink_0()
                            .child(inner)
                    }),
            );

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
            // 浮层放在最后，才能盖在内容之上
            .children(self.quick_visible.then(|| self.render_quick_open(cx)))
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
            .on_action(cx.listener(|this, _: &NextPage, _window, cx| {
                this.go_to_page(this.current_page + 1, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevPage, _window, cx| {
                let prev = this.current_page.saturating_sub(1);
                this.go_to_page(prev, cx);
            }))
            .on_action(cx.listener(|this, _: &FirstPage, _window, cx| {
                this.go_to_page(0, cx);
            }))
            .on_action(cx.listener(|this, _: &LastPage, _window, cx| {
                let last = this.bitmaps.len().saturating_sub(1);
                this.go_to_page(last, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.show_quick_open(window, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickPrev, _window, cx| {
                this.quick_move(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickNext, _window, cx| {
                this.quick_move(1, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickCancel, _window, cx| {
                this.cancel_quick(cx);
            }))
            // Ctrl + 滚轮缩放。不阻止容器自身的滚动（gpui 没有 preventDefault），
            // 所以缩完之后把当前页重新锚回顶部 —— 一举两得：既抵消了误滚，
            // 又让“缩放不跳位置”成为确定行为。
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                if !ev.modifiers.control {
                    return;
                }
                let dy = match ev.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => p.y.as_f32() / 40.0,
                };
                if dy == 0.0 {
                    return;
                }
                let next = if dy > 0.0 {
                    this.zoom * ZOOM_STEP
                } else {
                    this.zoom / ZOOM_STEP
                };
                this.set_zoom_anchored(next, cx);
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
            KeyBinding::new("pageup", PrevPage, None),
            KeyBinding::new("pagedown", NextPage, None),
            KeyBinding::new("ctrl-home", FirstPage, None),
            KeyBinding::new("ctrl-end", LastPage, None),
            // 快速打开。↑↓ / Esc 必须限定在浮层的按键上下文里，
            // 否则会抢走编辑器里的方向键。
            KeyBinding::new("ctrl-p", QuickOpen, None),
            KeyBinding::new("up", QuickPrev, Some("QuickOpen")),
            KeyBinding::new("down", QuickNext, Some("QuickOpen")),
            KeyBinding::new("escape", QuickCancel, Some("QuickOpen")),
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

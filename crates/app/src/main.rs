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
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Root, h_flex, v_flex};

use typst::foundations::Bytes;
use typst_engine::export::{RasterPage, pixel_per_pt_for_zoom, rasterize};
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

actions!(typst_live, [ZoomIn, ZoomOut, ZoomReset]);

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
const ZOOM_STEP: f32 = 1.25;

const DEMO_DOC: &str = r#"#set page(width: 15cm, height: auto, margin: 1.8cm)
#set text(size: 11pt)

= 实时编译演示

在**左边随便改点什么**，右边会在几毫秒内跟着变。

没有子进程，没有存盘，没有 IPC —— 编译器直接读内存里未保存的文本。

== 试试缩放

按 *Ctrl+=* 放大、*Ctrl+-* 缩小、*Ctrl+0* 回到 100%。

看状态栏：**排版次数不会变，光栅化次数才会变** —— 因为缩放只是重新出图，
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
    errors: Vec<String>,
}

struct Previewer {
    engine: EngineWorld,
    main_path: PathBuf,
    editor: Entity<InputState>,
    /// 排版结果。缩放时原样不动 —— 这是「排版 / 光栅化」两层的分界。
    doc: Option<Arc<PagedDocument>>,
    /// 缩放倍率。1.0 = 屏幕上「实际大小」（96 dpi）。
    zoom: f32,
    /// 光栅化产物（GPU 纹理）。每次排版或缩放后重建。
    bitmaps: Vec<Arc<RenderImage>>,
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
                this.recompile(cx);
            }
        });

        let mut this = Self {
            engine,
            main_path,
            editor,
            doc: None,
            zoom: 1.0,
            bitmaps: Vec::new(),
            status: Status::default(),
            _sub: sub,
        };
        this.recompile(cx);
        this
    }

    /// 一次完整的「编辑 → 重排版 → 重新出图」循环。
    ///
    /// 同步执行：排版只要几毫秒，开线程反而引入调度开销与状态同步的麻烦。
    fn recompile(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        let main_id = self.engine.entry().main();

        // ① 未保存文本进覆盖层
        self.engine
            .vfs_mut()
            .map_shadow(&self.main_path, Bytes::from_string(text.clone()));
        // ② 增量重解析（不是重建语法树）
        let outcome = self.engine.sources().feed_memory(main_id, &text);

        // ③ 排版
        let t = Instant::now();
        let result = typst::compile::<PagedDocument>(&self.engine);
        self.status.compile_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.status.compiles += 1;

        self.status.reparsed = outcome.reparsed.map(|r| r.len()).unwrap_or(0);
        self.status.text_bytes = text.len();

        match result.output {
            Ok(doc) => {
                let doc = Arc::new(doc);
                self.status.pages = doc.pages().len();
                self.doc = Some(doc);
                self.status.errors.clear();
            }
            Err(errors) => {
                // 编译失败：**不动 doc**，右边继续显示上一次成功的结果。
                self.status.errors = errors
                    .iter()
                    .take(6)
                    .map(|e| e.message.to_string())
                    .collect();
            }
        }

        self.rerasterize();
        cx.notify();
    }

    /// 从已排版的 `doc` 重新出图。**不碰编译器**。
    fn rerasterize(&mut self) {
        let Some(doc) = self.doc.clone() else {
            self.bitmaps.clear();
            return;
        };

        let ppp = pixel_per_pt_for_zoom(self.zoom);
        let t = Instant::now();
        let pages = rasterize(&doc, ppp);
        self.status.raster_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.status.rasters += 1;

        let total: usize = pages.iter().map(|p| p.rgba.len()).sum();
        self.bitmaps = pages.into_iter().filter_map(to_texture).collect();

        println!(
            "[typst-live] 光栅化 {} 页 @ {:.0}% ({:.2} px/pt)：{:.1} ms，纹理共 {:.1} MiB",
            self.bitmaps.len(),
            self.zoom * 100.0,
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

    fn render_statusbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let mut bar = h_flex()
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

        if self.status.errors.is_empty() {
            bar = bar.child(div().text_color(theme.success).child("✓ 无错误"));
        } else {
            bar = bar.child(div().text_color(theme.danger).child(format!(
                "✗ {} 个错误（预览保留上次成功结果）",
                self.status.errors.len()
            )));
        }
        bar
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
            .children(self.status.errors.iter().map(|e| div().child(e.clone())))
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_errors = !self.status.errors.is_empty();

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
                div().bg(gpui::white()).shadow_md().overflow_hidden().child(
                    img(ImageSource::Render(bitmap.clone()))
                        .w(px(size.width.0 as f32))
                        .h(px(size.height.0 as f32)),
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
            KeyBinding::new("ctrl--", ZoomOut, None),
            KeyBinding::new("ctrl-0", ZoomReset, None),
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

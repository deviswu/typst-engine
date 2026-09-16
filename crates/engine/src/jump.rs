//! 源码 ⇄ 显示区的双向定位（「跳转索引」）。
//!
//! 给两个方向用：
//! - **前向**（编辑区 → 显示区）：光标在源码第 `b` 字节 → 它在第几页、页内哪个位置
//! - **反向**（显示区 → 编辑区）：点在第 `p` 页的 `(x, y)` pt → 那是源码哪个字节
//!
//! # 为什么能做
//!
//! typst 的排版结果里**带着源码出处**：`Glyph.span` 是 `(Span, u16)`，
//! 其中 `u16` 是「相对该语法节点起点的字节偏移」（`typst-layout` 的
//! `SpanMapper::span_at` 就是这么填的）。所以遍历一次 `Page.frame`，
//! 每个字形都能同时拿到「页内坐标」与「源码字节」——不需要 fork、
//! 不需要 SyncTeX 那样的辅助文件、不需要重排版。
//!
//! # 为什么坐标用 pt，不用像素
//!
//! 索引只依赖**排版结果**，与缩放、滚动、光栅化都无关。于是它天然构成
//! 「排版 → 索引 → 光栅化」的第三层：
//!
//! ```text
//! 排版 (PagedDocument, pt)  ──►  本模块 (byte ⇄ 页/页内 pt)  ──►  光栅化 (纹理)
//!        每次敲键                     跳转时惰性构建一次              缩放/滚动时
//! ```
//!
//! 按 Ctrl+= 缩放时索引不该重建 —— 这是可以在状态栏上直接看到的断言
//! （「索引 N 次」不涨），也是 `zoom_is_irrelevant` 那条测试的意思。
//!
//! # 粒度：字形
//!
//! 索引的最小单位是**字形外接框**（`GlyphBox`），不是行、不是文本块。
//! 好处是前向能精确到字符（高亮框只盖住光标那一个字），反向能精确到
//! 双向都在「同一个字」上闭环 —— 这正是集成测试里那条往返性质：
//! `inverse(forward(b))` 必须回到 `b` 所在的行，通常就是同一个字。
//!
//! # 已知取舍
//!
//! - **旋转/倾斜的内容**：字形框取四角变换后的外接框（AABB），所以是近似
//! - **别的文件**（`#include` 进来的）：span 指向的不是主文件，直接跳过 ——
//!   外壳手上只有主文件的文本，跳过去也没有意义
//! - **图/线/形状**：不建索引（点击落在它们身上时走「就近吸附」兜底）
//! - **链接**：单独存一份矩形（`links()`），外壳用它做 Ctrl+单击打开；
//!   文档内部的 `Location` 链接暂时只报「不支持」
//! - **没有字形的源码**（`#set`、注释、未渲染的 `#let`）：前向查找退化成
//!   「就近吸附」——这是有意的，跳到一个附近的位置比什么都不做有用

use std::collections::HashMap;
use std::ops::Range;

use typst::layout::{Frame, FrameItem, Transform};
use typst::model::Destination;
use typst::syntax::{DiagSpan, Source, Span};
use typst_layout::PagedDocument;

use crate::syntax::range_of_diag_span;

/// 就近吸附时向前/向后最多看多少个条目。
///
/// 一个字形一般只覆盖 1–4 字节，所以 512 条足够跨过几 KB 的源码。
/// 设上限是为了让「光标停在一大片没有字形的区域里」也有确定的耗时。
const NEARBY_SCAN: usize = 512;

/// 一次定位的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    /// 0 起的页号。
    pub page: usize,
    /// 页内 pt 矩形 `[x0, y0, x1, y1]`，y 向下。
    pub rect: [f32; 4],
    /// 源码字节范围（**已对齐到字符边界**，可以直接切字符串）。
    pub byte: Range<usize>,
}

impl Anchor {
    /// 矩形中心。反向测试与「点哪里」用得上。
    pub fn center(&self) -> (f32, f32) {
        let [x0, y0, x1, y1] = self.rect;
        ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
    }
}

/// 一个字形在页面上的外接框，以及它的源码字节范围。
#[derive(Debug, Clone, Copy, PartialEq)]
struct GlyphBox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    start: u32,
    end: u32,
}

impl GlyphBox {
    fn anchor(&self, page: usize) -> Anchor {
        Anchor {
            page,
            rect: [self.x0, self.y0, self.x1, self.y1],
            byte: self.start as usize..self.end as usize,
        }
    }

    fn area(&self) -> f32 {
        ((self.x1 - self.x0) * (self.y1 - self.y0)).max(0.0)
    }

    /// 点到框的距离平方（框内为 0）。
    fn distance2(&self, x: f32, y: f32) -> f32 {
        let dx = (self.x0 - x).max(x - self.x1).max(0.0);
        let dy = (self.y0 - y).max(y - self.y1).max(0.0);
        dx * dx + dy * dy
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

/// 一个可点区域（链接）。
#[derive(Debug, Clone, PartialEq)]
pub struct LinkBox {
    /// 页内 pt 矩形 `[x0, y0, x1, y1]`，y 向下。
    pub rect: [f32; 4],
    /// 指向哪里：外链（`Url`）或文档内位置。
    pub dest: Destination,
}

/// 前向查找的索引项：按字节起点排序。
#[derive(Debug, Clone, Copy)]
struct Slot {
    start: u32,
    page: u32,
    glyph: u32,
}

/// 跳转索引。
#[derive(Debug, Default)]
pub struct LayoutIndex {
    /// 每页的字形框，页内顺序 = 绘制顺序。
    pages: Vec<Vec<GlyphBox>>,
    /// 每页的链接框。字与链接分开存：两者查找方式完全不同
    /// （字靠字节二分，链接靠点内判定）。
    links: Vec<Vec<LinkBox>>,
    /// 按字节起点排序的全表，前向查找用。
    slots: Vec<Slot>,
    /// `slots[..=i]` 里最大的字节终点。前向查找靠它**精确**判断
    /// 「还能不能有更早的条目覆盖这个字节」，而不是靠猜窗口大小。
    max_end: Vec<u32>,
}

impl LayoutIndex {
    /// 从已排版的文档建索引。
    ///
    /// 传进来的 `source` 必须是**排版时用的那一份**（外壳应当传
    /// `typst::World::source(world, main)` 拿到的、增量维护的那棵），
    /// 否则字节偏移对不上。
    pub fn build(doc: &PagedDocument, source: &Source) -> Self {
        let mut ctx = Build::new(source);

        let mut pages = Vec::with_capacity(doc.pages().len());
        let mut links = Vec::with_capacity(doc.pages().len());
        for page in doc.pages() {
            let mut out = PageOut::default();
            walk(&page.frame, Affine::IDENTITY, &mut ctx, &mut out);
            pages.push(out.glyphs);
            links.push(out.links);
        }

        let mut slots: Vec<Slot> = Vec::with_capacity(pages.iter().map(Vec::len).sum());
        for (page, glyphs) in pages.iter().enumerate() {
            for (glyph, g) in glyphs.iter().enumerate() {
                slots.push(Slot {
                    start: g.start,
                    page: page as u32,
                    glyph: glyph as u32,
                });
            }
        }

        // 大多数文档本来就按源码顺序出来，先探一下省掉一次排序 ——
        // 页眉页脚（同一段源码出现在每一页）会让顺序乱掉，那时才真排。
        if !slots.is_sorted_by_key(|s| s.start) {
            slots.sort_unstable_by_key(|s| s.start);
        }

        let mut max_end = Vec::with_capacity(slots.len());
        let mut running = 0u32;
        for slot in &slots {
            running = running.max(pages[slot.page as usize][slot.glyph as usize].end);
            max_end.push(running);
        }

        Self {
            pages,
            links,
            slots,
            max_end,
        }
    }

    /// 编辑区 → 显示区：源码第 `byte` 个字节落在哪一页的什么位置。
    ///
    /// 优先「精确命中」（该字节真有字形）。该字节没有字形时（`#set`、
    /// 注释、未渲染的代码）**就近吸附**到源码上最近的字形 —— 偏一点
    /// 比什么都不做有用；这在注释里写清楚了，不是意外行为。
    pub fn forward(&self, byte: usize) -> Option<Anchor> {
        if self.slots.is_empty() {
            return None;
        }

        // ── ① 精确命中：从「起点 ≤ byte」的最后一个往回走 ──
        // 同一起点的多条都在 i 之前，所以一次回退不会漏。
        // 页眉那种「同一段源码出现在每一页」的会命中多条 ——
        // 取页号最小、页内最靠上的那个（确定，不随抽屉顺序漂移）。
        let mut best: Option<Anchor> = None;
        let mut i = self.slots.partition_point(|s| (s.start as usize) <= byte);
        while i > 0 {
            i -= 1;
            let slot = self.slots[i];
            let glyph = self.glyph(slot);
            if (glyph.start as usize) <= byte && byte < glyph.end as usize {
                let anchor = glyph.anchor(slot.page as usize);
                let better = match &best {
                    None => true,
                    Some(old) => (anchor.page, anchor.rect[1]) < (old.page, old.rect[1]),
                };
                if better {
                    best = Some(anchor);
                }
            }
            if self.max_end[i] as usize <= byte {
                break;
            }
        }
        if best.is_some() {
            return best;
        }

        // ── ② 就近吸附：往两边看有限个条目，取源码距离最近的 ──
        // 平手时取**靠前**的那个：光标刚敲完回车停在空行上时，
        // 人想看的是上面那一段，不是下面那一段。
        let center = self.slots.partition_point(|s| (s.start as usize) <= byte);
        let lo = center.saturating_sub(NEARBY_SCAN);
        let hi = (center + NEARBY_SCAN).min(self.slots.len());

        let mut best: Option<(usize, Anchor)> = None;
        for slot in &self.slots[lo..hi] {
            let glyph = self.glyph(*slot);
            let distance = if byte < glyph.start as usize {
                glyph.start as usize - byte
            } else {
                byte.saturating_sub(glyph.end as usize)
            };
            let better = match &best {
                None => true,
                Some((old, _)) => distance < *old,
            };
            if better {
                best = Some((distance, glyph.anchor(slot.page as usize)));
            }
        }
        best.map(|(_, anchor)| anchor)
    }

    /// 显示区 → 编辑区：第 `page` 页上 `(x, y)` pt 处是源码的哪个字节。
    ///
    /// 命中多个框时取**面积最小**的（嵌套内容里更具体的那个）。
    /// 一点都没命中时（点在页边距、图上）**就近吸附**到本页最近的文字 ——
    /// 只要这一页有文字就不会给 `None`。
    pub fn inverse(&self, page: usize, x: f32, y: f32) -> Option<Anchor> {
        if !(x.is_finite() && y.is_finite()) {
            return None;
        }
        let glyphs = self.pages.get(page)?;

        let mut hit: Option<&GlyphBox> = None;
        let mut nearby: Option<(&GlyphBox, f32)> = None;

        for glyph in glyphs {
            if glyph.contains(x, y) {
                let better = match hit {
                    None => true,
                    Some(old) => glyph.area() < old.area(),
                };
                if better {
                    hit = Some(glyph);
                }
            } else {
                let distance = glyph.distance2(x, y);
                let better = match nearby {
                    None => true,
                    Some((_, old)) => distance < old,
                };
                if better {
                    nearby = Some((glyph, distance));
                }
            }
        }

        let glyph = hit.or(nearby.map(|(glyph, _)| glyph))?;
        Some(glyph.anchor(page))
    }

    /// 某一页上的链接（矩形已含 Group 变换）。
    ///
    /// 外壳拿它做「Ctrl+单击打开链接」。页号越界给空切片，不 panic。
    pub fn links(&self, page: usize) -> &[LinkBox] {
        self.links.get(page).map_or(&[], Vec::as_slice)
    }

    fn glyph(&self, slot: Slot) -> &GlyphBox {
        &self.pages[slot.page as usize][slot.glyph as usize]
    }

    /// 索引里的字形总数。状态栏与性能测试用 —— 它必须是「有字形的那些」，
    /// 不是源码里的字符数。
    pub fn glyph_count(&self) -> usize {
        self.slots.len()
    }

    /// 索引里的页数。
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

// ── 遍历排版结果 ──────────────────────────────────────────────

/// 一页上收集到的东西。
#[derive(Debug, Default)]
struct PageOut {
    glyphs: Vec<GlyphBox>,
    links: Vec<LinkBox>,
}

/// 二维仿射（pt，y 向下）：`x' = a·x + c·y + e`，`y' = b·x + d·y + f`。
///
/// 字段顺序与 typst 的 `Transform`、skia 的 `from_row(sx, ky, kx, sy, tx, ty)`
/// 一致 —— 这样和 `typst-render` 的坐标变换逐字对得上，不用再推一遍。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn translate(x: f32, y: f32) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }

    fn of(transform: &Transform) -> Self {
        Self {
            a: transform.sx.get() as f32,
            b: transform.ky.get() as f32,
            c: transform.kx.get() as f32,
            d: transform.sy.get() as f32,
            e: transform.tx.to_pt() as f32,
            f: transform.ty.to_pt() as f32,
        }
    }

    /// `self ∘ inner`：先做 `inner`，再做 `self`。
    ///
    /// 与 skia 的 `pre_concat` 同义（`typst-render` 就是用它的）。
    fn then(self, inner: Self) -> Self {
        Self {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// 把「条目局部坐标」里的一个矩形映射到页面坐标，返回它的外接框。
    ///
    /// 字形框与链接框走同一个函数：两者都是「在帧里占一块矩形」。
    ///
    /// 绝大部分内容只有平移（没有旋转/倾斜），那条路径直接算两个角 ——
    /// 建索引是**每个字形**都要走一遍的事，这里是热路径。
    /// （`b` / `c` 就是旋转与倾斜分量，与 `Transform` 的 `ky` / `kx` 对应。）
    fn map_rect(self, x0: f32, x1: f32, top: f32, bottom: f32) -> Option<[f32; 4]> {
        if self.b == 0.0 && self.c == 0.0 {
            let (ax0, ay0) = (self.a * x0 + self.e, self.d * top + self.f);
            let (ax1, ay1) = (self.a * x1 + self.e, self.d * bottom + self.f);
            if !(ax0.is_finite() && ay0.is_finite() && ax1.is_finite() && ay1.is_finite()) {
                return None;
            }
            return Some([ax0.min(ax1), ay0.min(ay1), ax0.max(ax1), ay0.max(ay1)]);
        }

        // 旋转/倾斜/镜像：四个角都算，取外接框
        let corners = [
            self.apply(x0, top),
            self.apply(x1, top),
            self.apply(x0, bottom),
            self.apply(x1, bottom),
        ];
        if corners
            .iter()
            .any(|(x, y)| !(x.is_finite() && y.is_finite()))
        {
            return None;
        }
        let xa = corners.iter().map(|(x, _)| *x).fold(f32::MAX, f32::min);
        let xb = corners.iter().map(|(x, _)| *x).fold(f32::MIN, f32::max);
        let ya = corners.iter().map(|(_, y)| *y).fold(f32::MAX, f32::min);
        let yb = corners.iter().map(|(_, y)| *y).fold(f32::MIN, f32::max);
        Some([xa, ya, xb, yb])
    }

    fn is_finite(self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.e.is_finite()
            && self.f.is_finite()
    }
}

/// 建索引时的共享状态：源码 + `Span → 节点范围` 的记忆化。
///
/// 记忆化分两级：一个 `HashMap` 兜底，外加一个**单条缓存** —— 同一段文本里
/// 连着几百个字形的 span 往往是同一个（一个语法节点），单条缓存就能
/// 把那几百次哈希查找省掉。实测这一步值 30% 左右的耗时。
struct Build<'a> {
    source: &'a Source,
    text: &'a str,
    cache: HashMap<Span, Option<Range<usize>>>,
    last: Option<(Span, Option<Range<usize>>)>,
}

impl<'a> Build<'a> {
    fn new(source: &'a Source) -> Self {
        Self {
            source,
            text: source.text(),
            cache: HashMap::new(),
            last: None,
        }
    }

    /// 字形的源码字节范围。
    ///
    /// `Span` 指向**另一个文件**（`#include`）或是 detached 时给 `None`。
    fn bytes(&mut self, span: (Span, u16)) -> Option<Range<usize>> {
        let (span, offset) = span;

        let node = if let Some((cached, range)) = &self.last {
            if *cached == span {
                range.clone()
            } else {
                self.node_range(span)
            }
        } else {
            self.node_range(span)
        }?;

        // 偏移可能因为 u16 饱和而超出节点（typst 自己承认这点），夹一下。
        let at = (node.start + offset as usize).min(node.end);
        char_range_at(self.text, at)
    }

    fn node_range(&mut self, span: Span) -> Option<Range<usize>> {
        let range = match self.cache.get(&span) {
            Some(hit) => hit.clone(),
            None => {
                let range = range_of_diag_span(self.source, &DiagSpan::from_span(span, None));
                self.cache.insert(span, range.clone());
                range
            }
        };
        self.last = Some((span, range.clone()));
        range
    }
}

/// 递归地走一帧，把文本项里的每个字形变成一条 `GlyphBox`。
///
/// 变换的累积方式与 `typst-render` 的 `State::pre_translate` /
/// `pre_concat` 完全一致（同一套复合顺序），所以坐标对得上渲染结果。
fn walk(frame: &Frame, acc: Affine, ctx: &mut Build<'_>, out: &mut PageOut) {
    for (pos, item) in frame.items() {
        let placed = Affine::translate(pos.x.to_pt() as f32, pos.y.to_pt() as f32);
        match item {
            FrameItem::Text(item) => {
                let aff = acc.then(placed);
                if !aff.is_finite() {
                    continue;
                }

                let size = item.size;
                let metrics = item.font.metrics();
                let ascent = metrics.ascender.at(size).to_pt() as f32;
                let descent = -metrics.descender.at(size).to_pt() as f32;
                if !(ascent.is_finite() && descent.is_finite()) {
                    continue;
                }

                // 字形沿基线依次排开。推进量是 advance，`x_offset` 只是
                // 绘制偏移（与 `TextItem::bbox` 的算法一致）。
                let mut cursor = 0.0f32;
                for glyph in &item.glyphs {
                    let advance = glyph.x_advance.at(size).to_pt() as f32;
                    let offset = glyph.x_offset.at(size).to_pt() as f32;
                    // 文本空间的 y 向上，帧空间的 y 向下 —— 所以 y_offset 要取负。
                    let rise = glyph.y_offset.at(size).to_pt() as f32;

                    let x0 = cursor + offset;
                    let x1 = x0 + advance;
                    cursor += advance;

                    if !(x0.is_finite() && x1.is_finite() && rise.is_finite()) {
                        continue;
                    }
                    let Some(byte) = ctx.bytes(glyph.span) else {
                        continue; // 别的文件、或 detached —— 跳过去没有意义
                    };

                    let top = -rise - ascent;
                    let bottom = -rise + descent;
                    let Some(rect) = aff.map_rect(x0, x1, top, bottom) else {
                        continue;
                    };

                    out.glyphs.push(GlyphBox {
                        x0: rect[0],
                        y0: rect[1],
                        x1: rect[2],
                        y1: rect[3],
                        start: byte.start as u32,
                        end: byte.end as u32,
                    });
                }
            }
            // 组：先按组的位置平移到它的原点，再上组自己的变换
            // （旋转/缩放绕组原点发生）。硬帧那套 container 变换只影响
            // 裁剪的坐标系，不影响内容落在页面的哪里，所以不用管。
            FrameItem::Group(group) => {
                let inner = placed.then(Affine::of(&group.transform));
                let child = acc.then(inner);
                if child.is_finite() {
                    walk(&group.frame, child, ctx, out);
                }
            }
            // 链接：一个矩形 + 一个去向。字与链接分开存 ——
            // 前者按字节二分，后者按点内判定，查询方式完全不同。
            FrameItem::Link(dest, size) => {
                let aff = acc.then(placed);
                if !aff.is_finite() {
                    continue;
                }
                let Some(rect) =
                    aff.map_rect(0.0, size.x.to_pt() as f32, 0.0, size.y.to_pt() as f32)
                else {
                    continue;
                };
                out.links.push(LinkBox {
                    rect,
                    dest: dest.clone(),
                });
            }
            // 形状 / 图片 / 标签：不建索引（见模块头的「已知取舍」）
            _ => {}
        }
    }
}

/// 字节 `byte` 所在**字符**的范围（对齐到字符边界）。
///
/// 为什么要对齐：`u16` 那个偏移是排版阶段累加出来的，理论上可能落在
/// 一个字符的中间。不对齐的话下游 `&text[..end]` 会直接 panic ——
/// 中文一个字 3 字节，这个坑很容易踩。
fn char_range_at(text: &str, byte: usize) -> Option<Range<usize>> {
    if byte >= text.len() {
        return None;
    }
    let mut start = byte;
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let len = text[start..].chars().next()?.len_utf8();
    Some(start..start + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_composes_like_a_matrix() {
        let a = Affine::translate(10.0, 20.0);
        let b = Affine::translate(1.0, 2.0);

        // 先 b 后 a = 平移到 (11, 22)
        assert_eq!(a.then(b).apply(0.0, 0.0), (11.0, 22.0));
        // 顺序反过来是另一回事（这里碰巧一样，所以再加一个缩放）
        let scale = Affine {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 3.0,
            e: 0.0,
            f: 0.0,
        };
        assert_eq!(scale.then(a).apply(1.0, 1.0), (22.0, 63.0));
        assert_eq!(a.then(scale).apply(1.0, 1.0), (12.0, 23.0));
    }

    #[test]
    fn a_transform_becomes_an_affine() {
        let t = Transform::translate(typst::layout::Abs::pt(5.0), typst::layout::Abs::pt(-7.0));
        let aff = Affine::of(&t);

        assert_eq!(aff.apply(0.0, 0.0), (5.0, -7.0));
        assert!(aff.is_finite());
    }

    #[test]
    fn char_ranges_align_to_boundaries() {
        let text = "a中文";

        assert_eq!(char_range_at(text, 0), Some(0..1));
        // 「中」占 3 字节：落在它中间的偏移要退回字符开头
        assert_eq!(char_range_at(text, 1), Some(1..4));
        assert_eq!(char_range_at(text, 2), Some(1..4));
        assert_eq!(char_range_at(text, 4), Some(4..7));
        assert_eq!(
            char_range_at(text, 3),
            Some(1..4),
            "「中」的末字节仍属「中」"
        );
    }

    #[test]
    fn char_ranges_clamp_instead_of_panicking() {
        assert_eq!(char_range_at("", 0), None);
        assert_eq!(char_range_at("abc", 3), None, "恰好越界");
        assert_eq!(char_range_at("abc", 999), None);
    }

    #[test]
    fn an_empty_index_answers_none() {
        let index = LayoutIndex::default();

        assert_eq!(index.forward(0), None);
        assert_eq!(index.inverse(0, 10.0, 10.0), None);
        assert_eq!(index.glyph_count(), 0);
    }

    #[test]
    fn inverse_of_a_page_that_does_not_exist_is_none() {
        let index = LayoutIndex {
            pages: vec![Vec::new()],
            links: vec![Vec::new()],
            slots: Vec::new(),
            max_end: Vec::new(),
        };

        assert_eq!(index.inverse(3, 10.0, 10.0), None);
        assert_eq!(index.inverse(0, f32::NAN, 0.0), None, "非有限坐标不算数");
        assert!(index.links(0).is_empty());
        assert!(index.links(7).is_empty(), "空的索引问哪一页都该是空切片");
    }
}

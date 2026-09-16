p = 'crates/app/src/main.rs'
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, f"匹配 {s.count(old)} 次：{old[:70]!r}"
    s = s.replace(old, new)


# ── 把 set_zoom 还原（别改签名），另加自适应版本 ──
rep("""    fn set_zoom(&mut self, zoom: f32, manual: Option<bool>) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        // 手动缩放就退出「适应宽度」；`None` = 系统自适应，保持原样
        if let Some(manual) = manual {
            self.zoom_fit = !manual;
        }
        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = zoom;""",
    """    /// 用户手动缩放（Ctrl+= / Ctrl+- / 滚轮）：退出「适应宽度」。
    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.zoom_fit = false;
        self.apply_zoom(zoom, cx);
    }

    /// 回到「适应宽度」（Ctrl+0）。
    fn fit_zoom(&mut self, cx: &mut Context<Self>) {
        self.zoom_fit = true;
        self.fit_zoom_to_viewport();
        self.touch_settings(cx);
        cx.notify();
    }

    /// 自适应改缩放：不动「手动/适应」开关，也不写设置（拖动分区时会频繁触发）。
    fn apply_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = zoom;""")

rep("""        // 只在真的变了才重建纹理（拖动时每帧都会算，但相等就不动手）
        if (fit - self.zoom).abs() > 0.005 {
            self.set_zoom(fit.clamp(ZOOM_MIN, ZOOM_MAX), None);
        }
    }""",
    """        // 只在真的变了才重建纹理（拖动时每帧都会算，但相等就不动手）
        let fit = fit.clamp(ZOOM_MIN, ZOOM_MAX);
        if (fit - self.zoom).abs() > 0.005 {
            self.zoom = fit;
            self.rerasterize();
        }
    }""")

# ── 常量与字段 ──
rep("""/// 前向跳转时把目标行放在视口顶部往下多少逻辑像素处 ——
/// 上面留一点，好看见「这是哪一段」。
const SYNC_MARGIN: f32 = 80.0;""",
    """/// 前向跳转时把目标行放在视口顶部往下多少逻辑像素处 ——
/// 上面留一点，好看见「这是哪一段」。
const SYNC_MARGIN: f32 = 80.0;

/// 预览里页面两侧留的边距（逻辑像素）。「适应宽度」时页宽 = 展示区宽 - 2×它。
const PREVIEW_MARGIN: f32 = 24.0;""")

rep("""    /// 缩放倍率。1.0 = 屏幕上「实际大小」（96 dpi）。
    zoom: f32,""",
    """    /// 缩放倍率。1.0 = 屏幕上「实际大小」（96 dpi）。
    zoom: f32,
    /// 缩放是不是「适应宽度」模式（默认开）：开着时每次渲染按展示区宽度重算，
    /// 所以拖动分区、改窗口大小页面都会跟着充满。手动缩放会关掉它。
    zoom_fit: bool,""")

rep("""            doc: None,
            // 上次的缩放从设置里恢复（还要夹一次，手改过的设置可能越界）
            zoom: settings.zoom.unwrap_or(1.0).clamp(ZOOM_MIN, ZOOM_MAX),""",
    """            doc: None,
            // 上次的缩放从设置里恢复（还要夹一次，手改过的设置可能越界）；
            // 但默认仍走「适应宽度」—— 用户要的是「页面始终充满展示区」
            zoom: settings.zoom.unwrap_or(1.0).clamp(ZOOM_MIN, ZOOM_MAX),
            zoom_fit: true,""")

# ── render 里调用自适应（要在 sync_visible_pages 之前，因为它可能重建纹理）──
rep("""        // 按当前视口补出/卸载纹理。内部只在可见范围真的变了才动手，""",
    """        // 「适应宽度」：按当前展示区宽度重算缩放（拖动分区/改窗口都会走到这）
        self.fit_zoom_to_viewport();

        // 按当前视口补出/卸载纹理。内部只在可见范围真的变了才动手，""")

# ── Ctrl+0 回到适应宽度；状态栏显示模式 ──
s = s.replace("""            .on_action(cx.listener(|this, _: &ZoomReset, _window, cx| {
                this.set_zoom(1.0, cx);
            }))""",
              """            .on_action(cx.listener(|this, _: &ZoomReset, _window, cx| {
                this.fit_zoom(cx);
            }))""")

rep("""                div()
                    .flex_shrink_0()
                    .text_color(theme.primary)
                    .child(format!("{:.0}%", self.zoom * 100.0)),""",
    """                div()
                    .flex_shrink_0()
                    .text_color(theme.primary)
                    .child(if self.zoom_fit {
                        format!("充满 {:.0}%", self.zoom * 100.0)
                    } else {
                        format!("{:.0}%", self.zoom * 100.0)
                    }),""")

# 菜单里「实际大小」改成「适应宽度」
s = s.replace('.item(PopupMenuItem::new("实际大小（Ctrl+0）").on_click(',
              '.item(PopupMenuItem::new("适应宽度（Ctrl+0）").on_click(')
s = s.replace("""                            &view,
                            |this, _, _window, cx| {
                                this.set_zoom(1.0, cx);
                            },
                        ))""",
              """                            &view,
                            |this, _, _window, cx| {
                                this.fit_zoom(cx);
                            },
                        ))""")

open(p, 'w', encoding='utf-8').write(s)
print("自适应缩放接好（不动 set_zoom 签名）")

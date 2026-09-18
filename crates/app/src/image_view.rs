//! 图片查看器：读文件字节 → gpui `img` 元素（内部解码 png/jpeg/gif/webp/bmp）。
//!
//! 移植自参考项目 `wu` 的 `src/image_view.rs`。保留它的两条设计：
//!
//! 1. **读文件放后台线程**：一张几十 MB 的图同步读会把界面卡住
//! 2. **解码留在 UI 线程**：`Image` 不是 `Send`，跨线程传字节即可

use std::path::{Path, PathBuf};
use std::sync::Arc;

// 只要用的那几个类型，**不要** `use gpui::*`：那会把 gpui 自己的
// `#[gpui::test]` 宏（名字就叫 `test`）带进来，于是本文件里的 `#[test]`
// 会解析到它头上（报「recursion limit reached while expanding #[test]」）。
use gpui::{
    Context, Image, ImageFormat, IntoElement, ObjectFit, ParentElement as _, Render, Styled as _,
    StyledImage as _, Window, div, img,
};
use gpui_component::{ActiveTheme as _, v_flex};

/// 图片查看视图：持有图片与格式，`render` 时交给 gpui 解码显示。
pub struct ImageView {
    path: PathBuf,
    image: Option<Arc<Image>>,
    error: Option<String>,
    loading: bool,
}

impl ImageView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let load_path = path.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let Some(format) = image_format_for(&load_path) else {
                        return Err("不支持的图片格式".to_string());
                    };
                    std::fs::read(&load_path)
                        .map(|bytes| (format, bytes))
                        .map_err(|e| format!("无法读取图片：{e}"))
                })
                .await;

            _ = weak.update(cx, |this, cx| {
                match result {
                    Ok((format, bytes)) => {
                        this.image = Some(Arc::new(Image::from_bytes(format, bytes)));
                        this.error = None;
                    }
                    Err(err) => this.error = Some(err),
                }
                this.loading = false;
                cx.notify();
            });
        })
        .detach();

        Self {
            path,
            image: None,
            error: None,
            loading: true,
        }
    }
}

/// 扩展名 → gpui 图片格式（纯函数，可在后台线程调用）。
fn image_format_for(path: &Path) -> Option<ImageFormat> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some(ImageFormat::Png),
        Some("jpg" | "jpeg") => Some(ImageFormat::Jpeg),
        Some("webp") => Some(ImageFormat::Webp),
        Some("gif") => Some(ImageFormat::Gif),
        Some("bmp") => Some(ImageFormat::Bmp),
        Some("svg") => Some(ImageFormat::Svg),
        _ => None,
    }
}

/// 这个扩展名是不是图片（给「点目录树里的文件该显示什么」用）。
pub fn is_image_path(path: &Path) -> bool {
    image_format_for(path).is_some()
}

impl Render for ImageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(image) = &self.image {
            div().size_full().min_h_0().p_4().child(
                img(image.clone())
                    .object_fit(ObjectFit::Contain)
                    .size_full(),
            )
        } else {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(if self.loading {
                    "加载中…".to_string()
                } else {
                    self.error.clone().unwrap_or_else(|| {
                        format!(
                            "无法显示图片：{}",
                            self.path.file_name().unwrap_or_default().to_string_lossy()
                        )
                    })
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_decides_the_format() {
        let cases = [
            ("a.png", Some(ImageFormat::Png)),
            ("a.PNG", Some(ImageFormat::Png)),
            ("a.jpeg", Some(ImageFormat::Jpeg)),
            ("a.jpg", Some(ImageFormat::Jpeg)),
            ("a.svg", Some(ImageFormat::Svg)),
            ("a.bmp", Some(ImageFormat::Bmp)),
            ("a.typ", None),
            ("没有后缀", None),
        ];

        for (name, want) in cases {
            assert_eq!(image_format_for(Path::new(name)), want, "{name}");
        }
    }

    #[test]
    fn is_image_path_matches_the_format_table() {
        assert!(is_image_path(Path::new("图/照片.webp")));
        assert!(!is_image_path(Path::new("doc.typ")));
    }
}

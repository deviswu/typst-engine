//! 设置持久化：窗口尺寸/位置、上次打开的文件、缩放、主题。
//!
//! # 为什么不拉 serde
//!
//! 就这么几项，手写 `key=value` 比引进一个序列化框架便宜。而且解析与渲染都是
//! **纯函数**：坏行、缺项、认不出的键都能直接单测，不用碰磁盘。
//!
//! # 文件在哪
//!
//! `%APPDATA%\typst-live\settings.conf`（Windows）、
//! `$HOME/.config/typst-live/settings.conf`（类 Unix）、都不行就落当前目录。
//!
//! # 一条硬规矩
//!
//! **设置文件永远不该拦住程序启动**：读不动、行坏了、键认不出，都只是
//! 「那一项没有」，不是错误。写不进去也只是报一行日志。

use std::path::{Path, PathBuf};

/// 窗口几何：`(x, y, 宽, 高)`，逻辑像素取整。
pub type WindowBox = (i32, i32, i32, i32);

pub const FILE_NAME: &str = "settings.conf";

/// 设置文件路径。
pub fn path() -> PathBuf {
    if let Some(dir) = std::env::var_os("APPDATA") {
        return PathBuf::from(dir).join("typst-live").join(FILE_NAME);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("typst-live")
            .join(FILE_NAME);
    }
    PathBuf::from(FILE_NAME)
}

/// 全部设置。每一项都是 `Option`：没有这项 = 用代码里的默认值。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub window: Option<WindowBox>,
    pub file: Option<PathBuf>,
    pub zoom: Option<f32>,
    /// 主题名（gpui-component 的 `ThemeRegistry` 里的名字）。
    pub theme: Option<String>,
    /// 深色模式。
    pub dark: Option<bool>,
}

impl Settings {
    /// 从默认位置读。
    pub fn load() -> Self {
        Self::load_from(&path())
    }

    /// 从指定文件读。文件不存在或读不动 → 全默认。
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// 写到默认位置（顺带建目录）。
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&path())
    }

    /// 写到指定文件。
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.render())
    }

    /// `key=value` 文本 → 设置。
    ///
    /// 空行与 `#` 开头的行是注释。认不出的键、解析不了的值一律**跳过** ——
    /// 手改过设置文件的人不该因此起不来。
    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();

            match key.trim() {
                "window" => out.window = parse_window(value),
                "file" => {
                    if !value.is_empty() {
                        out.file = Some(PathBuf::from(value));
                    }
                }
                "zoom" => out.zoom = value.parse::<f32>().ok().filter(|z| z.is_finite()),
                "theme" => {
                    if !value.is_empty() {
                        out.theme = Some(value.to_owned());
                    }
                }
                "dark" => {
                    out.dark = match value {
                        "1" | "true" | "yes" => Some(true),
                        "0" | "false" | "no" => Some(false),
                        _ => None,
                    }
                }
                _ => {}
            }
        }

        out
    }

    /// 设置 → `key=value` 文本。没有的项不写行。
    pub fn render(&self) -> String {
        let mut out = String::from("# typst-live 的设置。程序自己写，手改也认。\n");

        if let Some((x, y, w, h)) = self.window {
            out.push_str(&format!("window={x},{y},{w},{h}\n"));
        }
        if let Some(file) = &self.file {
            out.push_str(&format!("file={}\n", file.display()));
        }
        if let Some(zoom) = self.zoom {
            out.push_str(&format!("zoom={zoom}\n"));
        }
        if let Some(theme) = &self.theme {
            out.push_str(&format!("theme={theme}\n"));
        }
        if let Some(dark) = self.dark {
            out.push_str(&format!("dark={}\n", u8::from(dark)));
        }

        out
    }
}

/// `x,y,w,h` → 窗口几何。宽高必须为正 —— 否则窗口开出来是看不见的。
fn parse_window(value: &str) -> Option<WindowBox> {
    let numbers: Vec<i32> = value
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();

    match numbers[..] {
        [x, y, w, h] if w > 0 && h > 0 => Some((x, y, w, h)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            window: Some((100, 60, 1320, 880)),
            file: Some(PathBuf::from("/tmp/docs/报告.typ")),
            zoom: Some(1.25),
            theme: Some("Default Dark".to_owned()),
            dark: Some(true),
        }
    }

    #[test]
    fn render_and_parse_round_trip() {
        let text = sample().render();

        assert_eq!(Settings::parse(&text), sample());
    }

    #[test]
    fn an_empty_file_gives_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(
            Settings::parse("# 只有注释\n\n"),
            Settings::default(),
            "注释与空行不该产生任何项"
        );
    }

    /// 手改坏了也不该拦住启动：坏行、认不出的键、解析不了的值都跳过。
    #[test]
    fn broken_lines_are_skipped_not_fatal() {
        let settings = Settings::parse(
            "window=不知道什么东西\n\
             zoom=abc\n\
             未知的键=1\n\
             这一行没有等号\n\
             dark=也许\n\
             file=\n\
             zoom=2\n",
        );

        assert_eq!(settings.window, None, "解析不了就别当设置");
        assert_eq!(settings.file, None, "空路径等于没设");
        assert_eq!(settings.dark, None);
        assert_eq!(settings.zoom, Some(2.0), "后面那行好的该生效");
    }

    /// 窗口宽高为 0（或负）是无效的：那样的窗口开出来看不见。
    #[test]
    fn a_window_without_area_is_rejected() {
        assert_eq!(parse_window("0,0,0,0"), None);
        assert_eq!(parse_window("0,0,-100,600"), None);
        assert_eq!(parse_window("1,2,3"), None, "少一项也不行");
        assert_eq!(parse_window("-50,-20,900,600"), Some((-50, -20, 900, 600)));
    }

    /// 路径里的 `=`、空格、中文都必须原样保留（只按**第一个** = 切）。
    #[test]
    fn a_path_with_spaces_and_equals_survives() {
        let settings = Settings::parse("file=C:\\my docs\\a=b.typ\n");

        assert_eq!(settings.file, Some(PathBuf::from("C:\\my docs\\a=b.typ")));
    }

    /// 没有的项不写行 —— 不然文件里会堆一堆 `zoom=1` 这种「假装设过」的东西。
    #[test]
    fn absent_items_produce_no_lines() {
        let text = Settings::default().render();

        for key in ["window=", "file=", "zoom=", "theme=", "dark="] {
            assert!(!text.contains(key), "空的设置不该写 `{key}`：{text:?}");
        }
    }

    #[test]
    fn save_then_load_goes_through_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);

        sample().save_to(&path).expect("该能建目录并写进去");
        assert_eq!(Settings::load_from(&path), sample());

        // 读一个不存在的文件 → 默认，不是 panic
        assert_eq!(
            Settings::load_from(&dir.path().join("没有这个文件")),
            Settings::default()
        );
    }
}

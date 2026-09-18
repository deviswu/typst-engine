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
    /// AI 端点（OpenAI 兼容）。留空用内置默认（DeepSeek）。
    /// 环境变量 `AI_BASE_URL` 优先级更高 —— 便于临时指向本地模型。
    pub ai_base_url: Option<String>,
    /// AI 模型名。环境变量 `AI_MODEL` 优先。
    pub ai_model: Option<String>,
    /// AI 鉴权 Key。环境变量 `AI_API_KEY` 优先。
    ///
    /// 存明文是因为它就是个本地单机工具的配置；真要更严，用环境变量。
    pub ai_api_key: Option<String>,
    /// 主题名（gpui-component 的 `ThemeRegistry` 里的名字）。
    ///
    /// 亮/暗不用单独存：`Theme::apply_config` 会把配置放进对应那一侧，
    /// 名字本身就带着模式（`ThemeConfig.mode`）—— 存两份迟早会不一致。
    pub theme: Option<String>,
    /// 预览区背景样式：`solid`（纯色，默认）或 `grid`（网格）。
    pub preview_bg: Option<String>,
    /// 跟随光标：光标停下就把预览滚到对应位置（默认关 —— 它会抢滚动）。
    pub follow_cursor: Option<bool>,
    /// 编辑器代码折叠（`gpui-component` 默认就是开，所以「没这项」= 开）。
    pub folding: Option<bool>,
    /// 状态栏要不要显示性能指标（排版/光栅化/重解析/纹理……）。
    /// 默认**关**：状态栏保持精简，排查时才从「视图」菜单里打开。
    pub show_metrics: Option<bool>,
    /// 自动保存（打字停下 1 秒后写盘）。默认**开**。
    ///
    /// 只对「用户显式打开的文件」生效 —— 内置示例文档永远不写盘，那条守卫
    /// 与这个开关无关（见 `autosave::should_save`）。
    pub autosave: Option<bool>,
    /// 三块面板与两条栏的显隐（默认都显示，所以「没这项」= 开）。
    ///
    /// 录出来的画面**就是窗口本身** —— 所以「要干净画面」= 从「视图」菜单
    /// 把不想入镜的那些关掉，不需要另外做什么「录屏模式」。
    pub show_tree: Option<bool>,
    pub show_editor: Option<bool>,
    pub show_preview: Option<bool>,
    pub show_toolbar: Option<bool>,
    pub show_statusbar: Option<bool>,
    /// 录屏时录麦克风 / 录摄像头画中画（默认都开）。
    pub record_mic: Option<bool>,
    pub record_cam: Option<bool>,
    /// 最近用过的文件夹（目录树根），按最近使用排序。
    ///
    /// 一条一行（`recent_dir=...`）而不是拼成一行 —— 路径里可能有任何字符，
    /// 拼起来就得发明分隔符与转义规则。
    pub recent_dirs: Vec<PathBuf>,
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
    ///
    /// **先写临时文件再改名**（同目录，才能保证是同一卷上的原子替换）：
    /// 直接覆盖原文件的话，写一半崩/断电会把设置文件截断成半截 ——
    /// 解析再容错也救不回被砍掉的内容。
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }

        let mut tmp = path.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);

        std::fs::write(&tmp, self.render())?;
        // Windows 上 `rename` 覆盖已存在的文件是允许的（MoveFileEx + REPLACE_EXISTING）
        if let Err(err) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(())
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
                "file" if !value.is_empty() => out.file = Some(PathBuf::from(value)),
                "zoom" => out.zoom = value.parse::<f32>().ok().filter(|z| z.is_finite()),
                "theme" if !value.is_empty() => out.theme = Some(value.to_owned()),
                "preview_bg" if !value.is_empty() => out.preview_bg = Some(value.to_owned()),
                "follow_cursor" => out.follow_cursor = value.parse::<bool>().ok(),
                "folding" => out.folding = value.parse::<bool>().ok(),
                "show_metrics" => out.show_metrics = value.parse::<bool>().ok(),
                "autosave" => out.autosave = value.parse::<bool>().ok(),
                "show_tree" => out.show_tree = value.parse::<bool>().ok(),
                "show_editor" => out.show_editor = value.parse::<bool>().ok(),
                "show_preview" => out.show_preview = value.parse::<bool>().ok(),
                "show_toolbar" => out.show_toolbar = value.parse::<bool>().ok(),
                "show_statusbar" => out.show_statusbar = value.parse::<bool>().ok(),
                "record_mic" => out.record_mic = value.parse::<bool>().ok(),
                "record_cam" => out.record_cam = value.parse::<bool>().ok(),
                // 最近文件夹是多行：每行一条，读进来就往后排
                "recent_dir" if !value.is_empty() => {
                    if out.recent_dirs.len() < super::MAX_RECENT_DIRS {
                        out.recent_dirs.push(PathBuf::from(value));
                    }
                }
                "ai_base_url" if !value.is_empty() => out.ai_base_url = Some(value.to_owned()),
                "ai_model" if !value.is_empty() => out.ai_model = Some(value.to_owned()),
                "ai_api_key" if !value.is_empty() => out.ai_api_key = Some(value.to_owned()),
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
        if let Some(style) = &self.preview_bg {
            out.push_str(&format!("preview_bg={style}\n"));
        }
        if let Some(on) = self.follow_cursor {
            out.push_str(&format!("follow_cursor={on}\n"));
        }
        if let Some(on) = self.folding {
            out.push_str(&format!("folding={on}\n"));
        }
        if let Some(on) = self.show_metrics {
            out.push_str(&format!("show_metrics={on}\n"));
        }
        if let Some(on) = self.autosave {
            out.push_str(&format!("autosave={on}\n"));
        }
        for (key, on) in [
            ("show_tree", self.show_tree),
            ("show_editor", self.show_editor),
            ("show_preview", self.show_preview),
            ("show_toolbar", self.show_toolbar),
            ("show_statusbar", self.show_statusbar),
            ("record_mic", self.record_mic),
            ("record_cam", self.record_cam),
        ] {
            if let Some(on) = on {
                out.push_str(&format!("{key}={on}\n"));
            }
        }
        for dir in self.recent_dirs.iter().take(super::MAX_RECENT_DIRS) {
            out.push_str(&format!("recent_dir={}\n", dir.display()));
        }
        if let Some(url) = &self.ai_base_url {
            out.push_str(&format!("ai_base_url={url}\n"));
        }
        if let Some(model) = &self.ai_model {
            out.push_str(&format!("ai_model={model}\n"));
        }
        if let Some(key) = &self.ai_api_key {
            out.push_str(&format!("ai_api_key={key}\n"));
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
            preview_bg: Some("grid".to_owned()),
            follow_cursor: Some(true),
            folding: Some(false),
            show_metrics: Some(true),
            autosave: Some(false),
            show_tree: Some(false),
            show_editor: Some(true),
            show_preview: Some(true),
            show_toolbar: Some(false),
            show_statusbar: Some(true),
            record_mic: Some(false),
            record_cam: Some(true),
            recent_dirs: vec![
                PathBuf::from("D:/工作/钻头型号排名"),
                PathBuf::from("C:/Users/admin/文档"),
            ],
            ai_base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_owned()),
            ai_model: Some("qwen3".to_owned()),
            ai_api_key: Some("sk-本地测试".to_owned()),
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
        assert_eq!(settings.theme, None, "`dark` 这种旧键认不出就跳过");
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

    /// 最近文件夹：多行存、按顺序读回来；超过上限就截断。
    #[test]
    fn recent_dirs_are_stored_one_per_line_and_capped() {
        let many: Vec<PathBuf> = (0..crate::MAX_RECENT_DIRS + 5)
            .map(|i| PathBuf::from(format!("C:/dir{i}")))
            .collect();
        let text = Settings {
            recent_dirs: many,
            ..Settings::default()
        }
        .render();

        assert_eq!(text.matches("recent_dir=").count(), crate::MAX_RECENT_DIRS);

        let back = Settings::parse(&text);
        assert_eq!(back.recent_dirs.len(), crate::MAX_RECENT_DIRS);
        assert_eq!(
            back.recent_dirs[0],
            PathBuf::from("C:/dir0"),
            "顺序要保住（最近的在最前）"
        );
    }

    /// 路径里的空格、反斜杠、等号都不能破坏这一行。
    #[test]
    fn a_recent_dir_with_awkward_characters_survives() {
        let dir = PathBuf::from(r"C:\Users\admin\我的 文档\a=b");
        let text = Settings {
            recent_dirs: vec![dir.clone()],
            ..Settings::default()
        }
        .render();

        assert_eq!(Settings::parse(&text).recent_dirs, vec![dir]);
    }

    #[test]
    fn absent_items_produce_no_lines() {
        let text = Settings::default().render();

        for key in [
            "window=",
            "file=",
            "zoom=",
            "theme=",
            "preview_bg=",
            "follow_cursor=",
            "folding=",
            "show_metrics=",
            "autosave=",
            "recent_dir=",
            "ai_base_url=",
            "ai_model=",
            "ai_api_key=",
        ] {
            assert!(!text.contains(key), "空的设置不该写 `{key}`：{text:?}");
        }
    }

    /// 布尔项：`true`/`false` 都要能存能读，写坏的值当没有。
    #[test]
    fn boolean_items_round_trip_and_tolerate_garbage() {
        let text = Settings {
            follow_cursor: Some(true),
            folding: Some(false),
            autosave: Some(false),
            ..Settings::default()
        }
        .render();

        assert!(text.contains("follow_cursor=true"), "{text:?}");
        assert!(text.contains("folding=false"), "{text:?}");
        assert!(text.contains("autosave=false"), "{text:?}");

        let parsed = Settings::parse("follow_cursor=true\nfolding=false\nautosave=false\n");
        assert_eq!(parsed.follow_cursor, Some(true));
        assert_eq!(parsed.folding, Some(false));
        assert_eq!(parsed.autosave, Some(false));

        let broken = Settings::parse("follow_cursor=也许\nfolding=\nautosave=\n");
        assert_eq!(broken.follow_cursor, None, "解析不了就别当设置");
        assert_eq!(broken.folding, None);
        assert_eq!(broken.autosave, None);
    }

    /// 「没这项」= 用代码里的默认值。自动保存的默认是**开**，
    /// 所以一份空设置必须解析成 `None`（而不是 `Some(false)`）——
    /// 写成后者的话，老设置文件会把自动保存静默关掉。
    #[test]
    fn autosave_absent_means_none_not_false() {
        assert_eq!(Settings::parse("").autosave, None);
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

    /// 写盘是「临时文件 + 改名」：**不能留下 `.tmp` 残骸**，
    /// 也不能出现「原文件被截断」这种中间状态（那正是原子替换要防的事）。
    #[test]
    fn saving_replaces_atomically_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);

        Settings {
            zoom: Some(3.0),
            ..Settings::default()
        }
        .save_to(&path)
        .unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "改名之后不该留下临时文件：{leftovers:?}"
        );

        // 再存一次（覆盖已存在的文件）也要能成
        sample().save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), sample());
    }
}

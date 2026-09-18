//! 目录树：后台线程扫描（Send 的 `FsNode`）→ UI 线程转 `TreeItem`、按后缀给图标。
//!
//! 移植自参考项目 `wu` 的 `src/tree.rs`（同一套 gpui / gpui-component rev）。
//! 保留它的两条设计：
//!
//! 1. **扫描与 UI 类型分离**：`FsNode` 可 `Send`，扫描能在后台线程跑；
//!    `TreeItem` 是 gpui-component 的 UI 类型，只在 UI 线程构造
//! 2. **重型目录直接跳过**：打开一个大项目时，`target/` / `node_modules/`
//!    能把目录树撑到几万个节点，扫描和渲染都会拖死
//!
//! 相对 wu 的改动：加了递归深度上限（防符号链接环），
//! `is_heavy_dir` 多认一个 `.cargo`，并让 `finder` 复用它（两份跳过规则不许各写各的）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{AnyElement, IntoElement as _, Styled as _, hsla};
use gpui_component::tree::TreeItem;
use gpui_component::{Icon, IconName};

/// 递归扫描的深度上限。防符号链接环把启动卡死。
const MAX_DEPTH: usize = 12;

/// 应跳过的重型/无关目录名。
pub fn is_heavy_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".cargo"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | ".next"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".idea"
            | ".vs"
    )
}

/// 按文件后缀给一个带语义色的类型图标（目录树里一眼看出这是什么文件）。
pub fn file_type_icon(path: &Path) -> AnyElement {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let (icon, color) = match ext.as_str() {
        "typ" | "rs" | "py" | "js" | "ts" | "json" | "toml" | "yaml" | "yml" | "md" | "sh"
        | "bat" | "cmd" | "html" | "css" | "xml" | "scm" | "lock" => {
            (IconName::SquareTerminal, hsla(210., 0.80, 0.50, 1.))
        }
        "exe" | "msi" | "dll" | "so" | "bin" | "jar" | "wasm" => {
            (IconName::Play, hsla(150., 0.55, 0.42, 1.))
        }
        "pdb" | "obj" | "o" | "lib" | "a" | "pyc" => {
            (IconName::MemoryStick, hsla(270., 0.55, 0.55, 1.))
        }
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" => {
            (IconName::GalleryVerticalEnd, hsla(320., 0.65, 0.50, 1.))
        }
        "pdf" => (IconName::BookOpen, hsla(0., 0.75, 0.52, 1.)),
        "zip" | "tar" | "gz" | "7z" | "rar" | "bz2" | "xz" => {
            (IconName::HardDrive, hsla(38., 0.90, 0.48, 1.))
        }
        "xlsx" | "xls" | "csv" | "docx" | "doc" | "pptx" | "ppt" => {
            (IconName::ChartPie, hsla(160., 0.55, 0.40, 1.))
        }
        _ => (IconName::File, hsla(0., 0., 0.50, 1.)),
    };

    Icon::from(icon).text_color(color).into_any_element()
}

/// 目录树节点（后台线程扫描用；`Send`，与 UI 类型 `TreeItem` 分开）。
pub struct FsNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub children: Vec<FsNode>,
}

/// 递归扫描目录。跳过 `.git` 与常见重型构建目录。
pub fn scan_dir(dir: &Path) -> Vec<FsNode> {
    scan_dir_at(dir, 0)
}

/// 目录里的**子目录**（只一层，按名字排序），跳过隐藏目录与重目录。
///
/// 给「打开文件夹…」那个选择器用：一层一层点下去找目录，比一次扫全树快得多，
/// 也不用管展开状态。
pub fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            // 隐藏目录（`.git` 这种）与重目录不列 —— 要进也得有别的办法
            !name.starts_with('.') && !is_heavy_dir(&name)
        })
        .collect();

    out.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    out
}

/// 最顶层的可选位置：Windows 是各个存在的盘符，其它平台就是 `/`。
pub fn roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        (b'A'..=b'Z')
            .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
            .filter(|path| path.exists())
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

fn scan_dir_at(dir: &Path, depth: usize) -> Vec<FsNode> {
    let mut nodes = Vec::new();
    if depth > MAX_DEPTH {
        return nodes;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return nodes;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
        else {
            continue;
        };

        // 隐藏文件（除目录树自己要看的）不进树：`.gitignore` 这类东西点不开
        let is_dir = path.is_dir();
        if is_dir && is_heavy_dir(&name) {
            continue;
        }
        if name.starts_with('.') && name != ".gitignore" && name != ".gitattributes" {
            continue;
        }

        let children = if is_dir {
            scan_dir_at(&path, depth + 1)
        } else {
            Vec::new()
        };
        nodes.push(FsNode {
            path,
            name,
            is_dir,
            children,
        });
    }

    // 目录在前、同类按名字排 —— 次序必须确定，否则每次刷新列表都在跳
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    nodes
}

/// 扫描结果里的节点总数（日志用 —— 「扫了多少东西」得是个数字）。
pub fn count_nodes(nodes: &[FsNode]) -> usize {
    nodes
        .iter()
        .map(|node| 1 + count_nodes(&node.children))
        .sum()
}

/// 后台扫描结果 → UI 树条目（在 UI 线程执行）。
fn nodes_to_tree_items(nodes: Vec<FsNode>, expanded: &HashSet<String>) -> Vec<TreeItem> {
    nodes
        .into_iter()
        .map(|node| {
            let id = node.path.to_string_lossy().to_string();
            if node.is_dir {
                let children = nodes_to_tree_items(node.children, expanded);
                let is_expanded = expanded.contains(&id);
                TreeItem::new(id, node.name)
                    .children(children)
                    .expanded(is_expanded)
            } else {
                TreeItem::new(id, node.name)
            }
        })
        .collect()
}

/// 构建目录树条目：顶部一个「根目录」条目（默认展开），其下是根目录内容。
pub fn build_file_items(
    root: &Path,
    nodes: Vec<FsNode>,
    expanded: &HashSet<String>,
) -> Vec<TreeItem> {
    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string());

    vec![
        TreeItem::new(root.to_string_lossy().to_string(), root_name)
            .children(nodes_to_tree_items(nodes, expanded))
            .expanded(true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个临时目录：`a.typ`、`sub/b.typ`、`target/skip.typ`、`.hidden`
    fn fixture(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("typst_engine_tree_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("a.typ"), "").unwrap();
        std::fs::write(dir.join("sub/b.typ"), "").unwrap();
        std::fs::write(dir.join("target/skip.typ"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        dir
    }

    #[test]
    fn heavy_dirs_are_skipped() {
        for name in [".git", ".cargo", "target", "node_modules", "dist", "build"] {
            assert!(is_heavy_dir(name), "`{name}` 该被跳过");
        }
        for name in ["src", "docs", "assets", "crates"] {
            assert!(!is_heavy_dir(name), "`{name}` 不该被跳过");
        }
    }

    #[test]
    fn scanning_skips_target_and_hidden_files() {
        let dir = fixture("scan");
        let names =
            |nodes: &[FsNode]| -> Vec<String> { nodes.iter().map(|n| n.name.clone()).collect() };

        let top = scan_dir(&dir);
        let top_names = names(&top);

        assert!(top_names.contains(&"a.typ".to_string()), "{top_names:?}");
        assert!(top_names.contains(&"sub".to_string()), "{top_names:?}");
        assert!(
            !top_names.contains(&"target".to_string()),
            "重型目录该跳过：{top_names:?}"
        );
        assert!(
            !top_names.contains(&".hidden".to_string()),
            "隐藏文件该跳过：{top_names:?}"
        );

        // 目录在前，其余按名字排（次序确定）
        assert_eq!(top_names, vec!["sub".to_string(), "a.typ".to_string()]);

        // 子目录里该有 b.typ
        let sub = top.iter().find(|n| n.name == "sub").unwrap();
        assert_eq!(names(&sub.children), vec!["b.typ".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanning_a_missing_dir_is_empty_not_a_panic() {
        assert!(scan_dir(Path::new("definitely/not/here")).is_empty());
    }

    /// 展开状态要能跨刷新保住：重建条目时按 id 恢复 `expanded`。
    #[test]
    fn expansion_state_survives_a_rebuild() {
        let dir = fixture("expand");
        let nodes = scan_dir(&dir);
        let sub_id = dir.join("sub").to_string_lossy().to_string();

        let collapsed = build_file_items(&dir, nodes, &HashSet::new());
        assert!(
            !collapsed[0].children[0].is_expanded(),
            "默认不该展开子目录"
        );

        let nodes = scan_dir(&dir);
        let expanded: HashSet<String> = [sub_id].into_iter().collect();
        let rebuilt = build_file_items(&dir, nodes, &expanded);

        assert!(rebuilt[0].is_expanded(), "根条目总是展开的");
        assert!(rebuilt[0].children[0].is_expanded(), "展开状态该被恢复");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_root_entry_is_named_after_the_folder() {
        let dir = fixture("naming");
        let items = build_file_items(&dir, Vec::new(), &HashSet::new());

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].label.as_ref(),
            dir.file_name().unwrap().to_string_lossy()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 子目录列表：只列一层、按名字排序、跳过隐藏与重目录。
    #[test]
    fn subdirs_lists_only_visible_directories() {
        let dir = fixture("picker");
        let sub = dir.join("subdirs");
        std::fs::create_dir_all(sub.join("b")).unwrap();
        std::fs::create_dir_all(sub.join("a")).unwrap();
        std::fs::create_dir_all(sub.join(".hidden")).unwrap();
        std::fs::create_dir_all(sub.join("target")).unwrap();
        std::fs::write(sub.join("文件.typ"), "hi").unwrap();

        let dirs: Vec<String> = subdirs(&sub)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert_eq!(dirs, vec!["a", "b"], "该只列可见目录，且排序");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 读不了的目录给空表，不 panic。
    #[test]
    fn subdirs_of_a_missing_dir_is_empty() {
        assert!(subdirs(Path::new("C:/这个目录不存在-typst-live")).is_empty());
    }

    /// 顶层位置至少有一个（Windows 上是存在的盘符）。
    #[test]
    fn roots_is_never_empty() {
        assert!(!roots().is_empty());
    }
}

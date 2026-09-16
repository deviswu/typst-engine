//! 文件模糊查找。
//!
//! 纯逻辑、无 UI 依赖，所以能单测 —— 打分函数不写测试几乎必然在某个
//! 边界上给出反直觉的顺序，而「搜不到想要的文件」是最容易被归咎于
//! 「这功能不好用」而不是「打分函数有 bug」的那类问题。

use std::path::{Path, PathBuf};

/// 扫描时跳过的目录名。
const SKIP_DIRS: &[&str] = &[
    "target",
    ".git",
    ".cargo",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
];

/// 最大递归深度。防止符号链接环或异常深的目录把启动卡住。
const MAX_DEPTH: usize = 12;

/// 递归收集候选文件，返回**相对 `root`** 的路径。
///
/// 结果按路径排序，保证同样的目录布局给出同样的候选顺序
/// （不然每次 Ctrl+P 的列表顺序都会变，手感很差）。
pub fn scan(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();

            // 隐藏文件与常见的垃圾目录一律不看
            if name.starts_with('.') {
                continue;
            }

            if file_type.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else if file_type.is_file()
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                out.push(rel.to_path_buf());
                if out.len() >= limit {
                    out.sort();
                    return out;
                }
            }
        }
    }

    out.sort();
    out
}

/// 给一个候选打分。`None` 表示不匹配。
///
/// 规则：`query` 的字符必须**按顺序**出现在候选里（子序列匹配，不要求连续）。
/// 在此之上按「像不像用户想要的」加分：
///
/// - `+1`  每个命中字符
/// - `+8`  与上一个命中**相邻**（连续命中，`ab` 命中 `abc` 比命中 `a_b_c` 好）
/// - `+6`  命中在**词的边界**（`/ _ - . 空格` 之后，或大小写切换处）
/// - `+4`  命中在**文件名**部分（`src/util.rs` 里搜 `util` 应该比搜 `src` 更相关）
/// - 末尾按候选长度轻微惩罚，让短路径排前面
pub fn score(query: &str, candidate: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }

    let c: Vec<char> = candidate.chars().collect();
    // 文件名的起始字符下标（`src/util.rs` → 4）
    let name_start = candidate
        .rfind(['/', '\\'])
        .map(|byte| candidate[..byte].chars().count() + 1)
        .unwrap_or(0);

    let mut total = 0i32;
    let mut qi = 0usize;
    let mut prev_hit: Option<usize> = None;

    for (i, ch) in c.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if !ch.eq_ignore_ascii_case(&q[qi]) {
            continue;
        }

        total += 1;
        if prev_hit == Some(i.saturating_sub(1)) && i > 0 {
            total += 8;
        }
        if i == 0 || is_boundary(c[i - 1]) {
            total += 6;
        }
        if i >= name_start {
            total += 4;
        }
        prev_hit = Some(i);
        qi += 1;
    }

    if qi < q.len() {
        return None;
    }

    Some(total - (c.len() as i32) / 8)
}

fn is_boundary(prev: char) -> bool {
    matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ') || prev.is_uppercase()
}

/// 排序并取前 `limit` 个。
///
/// 同分时按路径字典序 —— 必须有确定的次序，否则同分项的先后会随
/// 内部存储顺序漂移，用户会觉得列表在「乱跳」。
pub fn rank(query: &str, candidates: &[PathBuf], limit: usize) -> Vec<PathBuf> {
    let mut scored: Vec<(i32, &PathBuf)> = candidates
        .iter()
        .filter_map(|p| score(query, &p.to_string_lossy()).map(|s| (s, p)))
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, p)| p.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_subsequence_matches() {
        assert!(score("utl", "src/util.rs").is_some(), "u-t-l 按序出现");
        assert!(score("srcutil", "src/util.rs").is_some(), "斜杠可以跳过");
    }

    #[test]
    fn out_of_order_does_not_match() {
        assert_eq!(score("lu", "src/util.rs"), None, "u 在 l 之前，不算匹配");
        assert_eq!(score("xyz", "src/util.rs"), None);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(score("UTIL", "src/util.rs").is_some());
        assert!(score("util", "src/UTIL.rs").is_some());
    }

    /// 连续命中要比散落命中得分高。
    #[test]
    fn consecutive_beats_scattered() {
        let tight = score("util", "util.rs").unwrap();
        let loose = score("util", "u_t_i_l.rs").unwrap();

        assert!(tight > loose, "连续 {tight} 应高于散落 {loose}");
    }

    /// 词首命中要比词中命中得分高。
    #[test]
    fn word_boundary_beats_middle_of_word() {
        let boundary = score("util", "src/util.rs").unwrap();
        let middle = score("util", "src/xutil.rs").unwrap();

        assert!(boundary > middle, "词首 {boundary} 应高于词中 {middle}");
    }

    /// 命中文件名比命中目录名更相关。
    #[test]
    fn filename_beats_directory() {
        let in_name = score("util", "src/util.rs").unwrap();
        let in_dir = score("util", "util/x.rs").unwrap();

        assert!(
            in_name > in_dir,
            "文件名命中 {in_name} 应高于目录名命中 {in_dir}"
        );
    }

    /// 空 query 匹配所有 —— 不输入时应当列全部。
    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(score("", "anything"), Some(0));
        assert_eq!(score("   ", "anything"), Some(0), "纯空格等同空");
    }

    #[test]
    fn ranking_puts_the_best_first() {
        let files = paths(&[
            "chapters/intro.typ",
            "util/helpers.rs",
            "src/util.rs",
            "main.typ",
        ]);

        let ranked = rank("util", &files, 10);

        assert_eq!(ranked.len(), 2, "只有两个含 u-t-i-l 的");
        assert_eq!(
            ranked[0],
            PathBuf::from("src/util.rs"),
            "文件名精确命中该排第一，实际 {ranked:?}"
        );
    }

    #[test]
    fn ranking_respects_the_limit() {
        let files = paths(&["a.typ", "b.typ", "c.typ", "d.typ"]);

        assert_eq!(rank("typ", &files, 2).len(), 2);
    }

    /// 同分时次序必须确定 —— 否则列表会随内部顺序漂移。
    #[test]
    fn ties_are_broken_deterministically() {
        let files = paths(&["b/x.typ", "a/x.typ", "c/x.typ"]);

        let first = rank("x.typ", &files, 3);
        let second = rank("x.typ", &files, 3);

        assert_eq!(first, second, "两次排序结果必须一致");
        assert_eq!(first[0], PathBuf::from("a/x.typ"), "字典序小的在前");
    }

    #[test]
    fn scanning_finds_files_and_skips_noise() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("chapters")).unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("main.typ"), "").unwrap();
        std::fs::write(dir.path().join("chapters/intro.typ"), "").unwrap();
        std::fs::write(dir.path().join("target/debug/junk.typ"), "").unwrap();
        std::fs::write(dir.path().join(".git/config"), "").unwrap();
        std::fs::write(dir.path().join(".hidden.typ"), "").unwrap();

        let found = scan(dir.path(), 100);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();

        assert!(names.contains(&"main.typ".to_string()), "{names:?}");
        assert!(
            names.contains(&"chapters/intro.typ".to_string()),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("target")),
            "该跳过 target：{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(".git")),
            "该跳过 .git：{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("hidden")),
            "该跳过隐藏文件：{names:?}"
        );
    }

    #[test]
    fn scanning_respects_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(dir.path().join(format!("f{i}.typ")), "").unwrap();
        }

        assert!(scan(dir.path(), 5).len() <= 5);
    }

    #[test]
    fn scanning_a_missing_directory_is_empty_not_a_panic() {
        assert!(scan(Path::new("definitely/not/here"), 10).is_empty());
    }

    /// 中文文件名要能搜到 —— 用拼音首字母或汉字都行。
    #[test]
    fn cjk_filenames_work() {
        assert!(score("报告", "文档/报告.typ").is_some());
        assert!(score("文档", "文档/报告.typ").is_some());
    }
}

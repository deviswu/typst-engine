//! Typst 补全（IntelliSense 的最小可用版）。
//!
//! # 为什么现在才做
//!
//! 库侧的钩子一直留着（`gpui_component::input::CompletionProvider`：谁给候选、
//! 什么时候触发，都由应用说了算），缺的是**候选来源**。那部分放在引擎里
//! （`typst_engine::syntax::{builtin_names, definitions}`）—— 补全要知道
//! 「Typst 有哪些内置」和「这份文档自己 let 了什么」，而这两件事都只有引擎知道。
//!
//! # 触发规则（刻意简陋，但可预期）
//!
//! `#` 之后正在敲标识符字符时触发；候选按已敲的前缀过滤。
//! **不做**「按语法上下文猜该补什么」（那需要真正的语义分析），
//! 也不在注释里弹（行首有 `//` 就放过）。
//!
//! 补全插入用 `text_edit`（明确的替换范围）而不是 `insert_text`：
//! 后者的语义是「在光标处插入」，会把已经敲的前缀留着 —— 那会得到
//! `tab` + `table` = `tabtable`。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use anyhow::Result;
use gpui::{Context, Task, Window};
use gpui_component::input::{CompletionProvider, InputState, RopeExt as _};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Position, Range as LspRange, TextEdit,
};
use ropey::Rope;

/// 候选太多反而找不到（弹窗也会长到离谱）。
const MAX_ITEMS: usize = 60;

/// 往前最多扫这么多个字符去找前缀 —— 再多就不该有补全了。
const MAX_PREFIX: usize = 64;

/// 本文档定义的名字（`#let` / `#import`）由外壳在每次重编译后刷新。
pub type SharedNames = Rc<RefCell<Vec<String>>>;

pub struct TypstCompletion {
    local: SharedNames,
}

impl TypstCompletion {
    pub fn new(local: SharedNames) -> Self {
        Self { local }
    }

    /// 光标前那一小段文本，以及它的起始字节偏移。`None` = 这里不该补全。
    fn prefix_at(&self, source: &str, offset: usize) -> Option<(String, usize)> {
        if offset > source.len() {
            return None;
        }
        let head = source.get(..offset)?;

        // 1) 往前收标识符字符
        let mut start = offset;
        for (index, ch) in head.char_indices().rev() {
            if is_ident_char(ch) {
                start = index;
                if offset - start >= MAX_PREFIX {
                    return None;
                }
            } else {
                break;
            }
        }

        // 2) 再往前必须正好是 `#`（Typst 的函数调用位），否则不弹
        if source.get(..start)?.ends_with('#') {
            Some((head[start..].to_string(), start))
        } else {
            None
        }
    }

    /// 注释行里不弹（`// 这是一句 #注释` 不需要补全）。
    ///
    /// ⚠️ 偏移必须先夹到**字符边界**：编辑器给的是字节偏移，而中文文档里
    /// 它可能落在某个字的中间（「重解析」这类回调都出现过）。直接 `source[..offset]`
    /// 会 panic —— 引擎那边 `line_col` 咬过同一口。
    fn in_comment(&self, source: &str, offset: usize) -> bool {
        let offset = source.floor_char_boundary(offset.min(source.len()));
        let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
        source[line_start..offset].contains("//")
    }

    fn candidates(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let source = text.to_string();
        let offset = source.floor_char_boundary(offset.min(source.len()));
        if self.in_comment(&source, offset) {
            return Vec::new();
        }
        let Some((prefix, start)) = self.prefix_at(&source, offset) else {
            return Vec::new();
        };

        let lower = prefix.to_lowercase();
        let end = text.offset_to_position(offset.min(text.len()));
        let head = text.offset_to_position(start);

        // 本文档定义的名字排前面：它们是「刚写的那个函数」，
        // 比内置的 `table` / `figure` 更可能是用户当下想敲的。
        let mut items: Vec<(u8, CompletionItem)> = Vec::new();
        // 同一个名字可能从两档里来（`table` 既在常用表里、也在内置表里）——
        // 下拉里出现两个一模一样的条目会让人以为程序坏了。
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |rank: u8, name: &str, detail: &str, kind: CompletionItemKind| {
            if !seen.insert(name.to_string()) {
                return;
            }
            let name_lower = name.to_lowercase();
            if !lower.is_empty() && !name_lower.starts_with(&lower) {
                return;
            }
            items.push((
                rank,
                CompletionItem {
                    label: name.to_string(),
                    kind: Some(kind),
                    detail: Some(detail.to_string()),
                    // 明确的替换范围：把已经敲的前缀换掉，而不是接在后面
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: LspRange {
                            start: Position::new(head.line, head.character),
                            end: Position::new(end.line, end.character),
                        },
                        new_text: name.to_string(),
                    })),
                    ..CompletionItem::default()
                },
            ));
        };

        // 排序：本文档定义 > 常用内置 > 关键字 > 其余内置。
        //
        // 「常用」这一档不是装饰：`#` 刚打出来时前缀是空的，若全按字母序，
        // 下拉里排前 60 个的会是一堆 `a` 开头的名字，`table` / `figure` 根本看不见。
        for name in self.local.borrow().iter() {
            push(0, name, "本文档定义", CompletionItemKind::FUNCTION);
        }
        for name in typst_engine::syntax::COMMON_NAMES {
            push(1, name, "常用", CompletionItemKind::FUNCTION);
        }
        for keyword in typst_engine::syntax::KEYWORDS {
            push(2, keyword, "关键字", CompletionItemKind::KEYWORD);
        }
        for name in typst_engine::syntax::builtin_names() {
            push(3, name, "Typst 内置", CompletionItemKind::FUNCTION);
        }

        // 同档按名字排序 → 下拉里的次序稳定（不会每次重排）
        items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.cmp(&b.1.label)));
        items.truncate(MAX_ITEMS);
        items.into_iter().map(|(_, item)| item).collect()
    }
}

impl CompletionProvider for TypstCompletion {
    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        _cx: &mut Context<InputState>,
    ) -> bool {
        // 敲的是标识符字符（`#` 之后的第一下当然也算）就试一次；
        // 真正「该不该弹」由 `candidates` 按上下文（前面是不是 `#`）决定。
        new_text.chars().next_back().is_some_and(is_ident_char)
    }

    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        Task::ready(Ok(CompletionResponse::Array(self.candidates(text, offset))))
    }
}

/// 标识符字符：Typst 的名字里可以有字母、数字、下划线和连字符（`font-family` 这种）。
fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> TypstCompletion {
        TypstCompletion::new(Rc::new(RefCell::new(vec!["我的函数".to_string()])))
    }

    fn labels(items: &[CompletionItem]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    /// `#ta` → 该给出以 ta 开头的那批（`table` 在里头）。
    #[test]
    fn completes_after_hash() {
        let items = provider().candidates(&Rope::from_str("#ta"), 3);

        assert!(
            labels(&items).contains(&"table".to_string()),
            "{:?}",
            labels(&items)
        );
    }

    /// 没有 `#` 的普通正文里不弹（`t` 这种前缀会命中几百个名字）。
    #[test]
    fn stays_quiet_in_plain_text() {
        assert!(
            provider()
                .candidates(&Rope::from_str("这是一段普通文字 ta"), 20)
                .is_empty()
        );
    }

    /// 偏移落在汉字**中间**也不能 panic。
    ///
    /// 这是个回归测试：初版 `in_comment` 直接 `source[..offset]` 切片，
    /// 而字节 13 在 `这是一段普通文字` 的第五个字中间 —— 一测就 panic。
    /// 引擎那边 `line_col` 咬过同一口（`floor_char_boundary`）。
    #[test]
    fn an_offset_inside_a_multibyte_char_does_not_panic() {
        let text = Rope::from_str("这是一段普通文字");
        for offset in 0..=text.len() {
            let _ = provider().candidates(&text, offset);
        }
    }

    /// 注释行里不弹。
    #[test]
    fn stays_quiet_in_comments() {
        let text = Rope::from_str("// 这里写 #ta 也不该弹\n");
        assert!(provider().candidates(&text, 16).is_empty());
    }

    /// 本文档定义的名字排在 Typst 内置前面。
    #[test]
    fn local_names_come_first() {
        let text = Rope::from_str("#let 我的函数 = 1\n#");
        let items = provider().candidates(&text, text.len());

        let local = items.iter().position(|i| i.label == "我的函数");
        let builtin = items.iter().position(|i| i.label == "table");
        assert!(local.is_some(), "本地名字该出现：{:?}", labels(&items));
        assert!(builtin.is_some(), "内置名字该出现");
        assert!(local.unwrap() < builtin.unwrap(), "本地名字该排在前面");
    }

    /// 替换范围必须盖住已敲的前缀 —— 否则会得到 `tabtable`。
    #[test]
    fn the_edit_range_covers_the_typed_prefix() {
        let text = Rope::from_str("#tab");
        let items = provider().candidates(&text, 4);
        let table = items
            .iter()
            .find(|i| i.label == "table")
            .expect("该有 table");

        let Some(CompletionTextEdit::Edit(edit)) = table.text_edit.as_ref() else {
            panic!("必须用 text_edit 给明确范围");
        };
        assert_eq!(edit.range.start.character, 1, "范围从 `#` 之后开始");
        assert_eq!(edit.range.end.character, 4);
        assert_eq!(edit.new_text, "table");
    }

    /// `#` 刚打出来（还没有前缀）也要能弹。
    #[test]
    fn an_empty_prefix_still_offers_everything() {
        let items = provider().candidates(&Rope::from_str("#"), 1);

        assert!(!items.is_empty());
        assert!(items.len() <= MAX_ITEMS);
    }

    /// 中文/多字节前缀不能 panic，也不能错位。
    #[test]
    fn multibyte_prefix_is_safe() {
        // 本地那份名字是按「重编译后刷新」来的，所以这里直接喂一个中文名
        let provider = TypstCompletion::new(Rc::new(RefCell::new(vec!["中文名".to_string()])));
        let text = Rope::from_str("#let 中文名 = 1\n#中文");
        let items = provider.candidates(&text, text.len());

        assert!(
            items.iter().any(|i| i.label == "中文名"),
            "{:?}",
            labels(&items)
        );
    }

    /// 下拉里不能出现两个一模一样的条目（`table` 既在常用表里、也在内置表里）。
    #[test]
    fn candidates_are_unique() {
        for prefix in ["", "t", "ta", "中文"] {
            let text = Rope::from_str(&format!("#{prefix}"));
            let labels = labels(&provider().candidates(&text, text.len()));
            let mut unique = labels.clone();
            unique.sort();
            unique.dedup();

            assert_eq!(labels.len(), unique.len(), "有重复候选：{labels:?}");
        }
    }

    /// 极端输入（越界偏移、超长前缀）不该 panic。
    #[test]
    fn odd_input_does_not_panic() {
        let text = Rope::from_str("#ta");
        let _ = provider().candidates(&text, 999);
        let long = Rope::from_str(&format!("#{}", "a".repeat(100)));
        assert!(
            provider().candidates(&long, long.len()).is_empty(),
            "超长前缀不补全"
        );
    }
}

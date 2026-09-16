//! 文本统计。
//!
//! 放在引擎而不是外壳里：它是纯函数、UI 无关，而且**可测** ——
//! 词数这种东西不写测试几乎必然会在某个中文/标点边界上算错。

/// 文本规模。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextStats {
    /// 字符数（不含换行与空白）。
    pub chars: usize,
    /// 词数。见 [`text_stats`] 的计数规则。
    pub words: usize,
    /// 行数。
    pub lines: usize,
}

impl TextStats {
    /// 给状态栏用的紧凑写法：`1,234 字 · 567 词 · 89 行`。
    pub fn summary(&self) -> String {
        format!(
            "{} 字 · {} 词 · {} 行",
            group(self.chars),
            group(self.words),
            group(self.lines)
        )
    }
}

/// 统计文本。
///
/// **词数规则**（这是唯一有歧义的地方，所以写清楚）：
/// - 每个中日韩字符算**一个词** —— 中文不写空格，按空格切词对中文等于没切
/// - 拉丁字母/数字的**连续串**算一个词
/// - 词内的连字符与撇号不断词：`well-known`、`don't`
///
/// 字符数不含换行与空白字符 —— 那是「篇幅」的直觉含义。
pub fn text_stats(text: &str) -> TextStats {
    let mut chars = 0;
    let mut words = 0;
    let mut in_latin_word = false;

    for c in text.chars() {
        if c.is_whitespace() {
            in_latin_word = false;
            continue;
        }
        chars += 1;

        if is_cjk(c) {
            words += 1;
            in_latin_word = false;
        } else if c.is_alphanumeric() {
            if !in_latin_word {
                words += 1;
                in_latin_word = true;
            }
        } else if (c == '\'' || c == '-') && in_latin_word {
            // 词内标点：不断词
        } else {
            in_latin_word = false;
        }
    }

    TextStats {
        chars,
        words,
        lines: text.lines().count(),
    }
}

/// 中日韩字符。
///
/// 注意必须**先于** `char::is_alphanumeric` 判断：在 Rust 里
/// `'中'.is_alphanumeric()` 是 `true`（它是 Letter），
/// 顺序反了的话中文会走拉丁分支，一整段中文只算一个词。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF     // 日文假名
        | 0x3400..=0x4DBF   // 中日韩扩展 A
        | 0x4E00..=0x9FFF   // 中日韩基本区
        | 0xF900..=0xFAFF   // 兼容表意文字
        | 0xAC00..=0xD7AF   // 韩文音节
    )
}

/// 千位分隔，纯为了状态栏好读。
fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_all_zeros() {
        assert_eq!(text_stats(""), TextStats::default());
    }

    #[test]
    fn latin_words_split_on_whitespace() {
        let s = text_stats("hello world foo");

        assert_eq!(s.words, 3);
        assert_eq!(s.chars, 13, "字母 11 + 空格不算");
        assert_eq!(s.lines, 1);
    }

    /// 中文不写空格 —— 每个汉字算一个词。
    #[test]
    fn each_cjk_character_is_a_word() {
        let s = text_stats("中文测试");

        assert_eq!(s.words, 4, "四个汉字 = 四个词");
        assert_eq!(s.chars, 4);
    }

    /// ★ 这条钉住「先判 CJK 再判 alphanumeric」的顺序。
    /// 顺序反了的话整段中文只算一个词。
    #[test]
    fn a_long_chinese_sentence_does_not_collapse_into_one_word() {
        let s = text_stats("这是一段比较长的中文，用来确认不会被当成一个词。");

        assert!(s.words > 10, "中文段落只算出 {} 个词，顺序搞反了", s.words);
    }

    #[test]
    fn mixed_chinese_and_latin() {
        let s = text_stats("Typst 是一个排版系统");

        // Typst = 1, 是一个排版系统 = 7
        assert_eq!(s.words, 8);
    }

    #[test]
    fn intra_word_punctuation_does_not_split() {
        assert_eq!(text_stats("don't").words, 1);
        assert_eq!(text_stats("well-known").words, 1);
        assert_eq!(text_stats("a-b c").words, 2);
    }

    #[test]
    fn punctuation_alone_is_not_a_word() {
        let s = text_stats("!!! ??? ...");

        assert_eq!(s.words, 0);
        assert_eq!(s.chars, 9, "标点算字符但不算词");
    }

    #[test]
    fn numbers_count_as_words() {
        assert_eq!(text_stats("1 2 3").words, 3);
        // 实测记录：点号算断词，所以 v0.15.1 是三个词
        assert_eq!(text_stats("v0.15.1").words, 3);
    }

    #[test]
    fn lines_count_newlines_not_trailing_whitespace() {
        assert_eq!(text_stats("a\nb\nc").lines, 3);
        assert_eq!(text_stats("a\nb\nc\n").lines, 3, "结尾换行不算多一行");
        assert_eq!(text_stats("a").lines, 1);
    }

    #[test]
    fn whitespace_is_excluded_from_the_character_count() {
        assert_eq!(text_stats("a b\tc\nd").chars, 4);
    }

    #[test]
    fn summary_is_grouped_and_readable() {
        let s = TextStats {
            chars: 1234,
            words: 567,
            lines: 89,
        };

        assert_eq!(s.summary(), "1,234 字 · 567 词 · 89 行");
    }

    #[test]
    fn grouping_handles_short_and_boundary_lengths() {
        assert_eq!(group(0), "0");
        assert_eq!(group(7), "7");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(1000000), "1,000,000");
    }
}

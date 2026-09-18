//! 磁盘上的那一份变了，什么时候该把它拿回来。
//!
//! 触发方式是**轮询**（[`POLL`]，500 ms 一拍）：不引 `notify`、不建线程、也不用
//! 把事件从别的线程搬回 gpui 主线程。一次 `metadata()` 就够，而且「编辑器原子保存」
//! （写临时文件再 rename）天然被「长度变了」兜住 —— 何况我们自己写盘之后会顺手
//! 更新一次指纹（见 `Previewer::write_to_disk`），所以绝不会把自己的保存当成外部改动。
//!
//! 单独成模块的理由与 `autosave` 一样：**判错的后果是把用户刚敲的字盖掉**。
//! 所以「该不该重读」是个纯函数，每条分支都有单测钉住。

use std::path::Path;
use std::time::{Duration, SystemTime};

/// 多久查一次。
///
/// 500 ms 是「人感觉不到延迟」与「不白烧 CPU」之间的取中值：一次 `metadata()`
/// 在本地盘上是几微秒，而且放在后台线程取（见 `Previewer::spawn_disk_watch`）。
pub const POLL: Duration = Duration::from_millis(500);

/// 发现变化之后再等多久才认为「写完了」。
///
/// 编辑器保存是**两步**（写临时文件 + rename），中间那一刻读到的可能是半截文件；
/// 定时器唤醒的脚本、`cp` 一个几百 KB 的稿子也都有这个过程。
/// 所以等到指纹连续两拍不动为止，而不是一发现变化就立刻读。
pub const SETTLE: Duration = Duration::from_millis(200);

/// 防抖最多等几拍（等不出「不动」就放弃这一轮，下一拍重来）。
///
/// 8 × 200 ms = 1.6 s：足够任何一次「编辑器保存」落定。真有个进程在持续改这个
/// 文件（比如日志），这个循环也不至于卡在里面转不出来。
pub const SETTLE_ROUNDS: u32 = 8;

/// 磁盘上那一份的指纹：长度 + 修改时间。
///
/// 两个都要：**原子保存**（临时文件改名）长度常常不变而 mtime 变；反过来，
/// 有些工具会把 mtime 打回去（`touch -d`、`rsync -t`）而内容真的变了 —— 长度兜住它。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stamp {
    pub len: u64,
    pub mtime: Option<SystemTime>,
}

impl Stamp {
    /// 取指纹。文件不在（或取不到属性）→ `None`。
    ///
    /// 「不存在」是一种状态，必须与「还没查过」分开：`None → Some` 就是
    /// 「文件被创建出来了」，那也是一次该重读的变化 —— `typst-live 新稿.typ`
    /// 打开一个还不存在的文件是允许的（第一次保存时它才出现）。
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            mtime: meta.modified().ok(),
        })
    }

    /// 两份指纹是不是同一份。
    pub fn differs(&self, other: &Self) -> bool {
        self.len != other.len || self.mtime != other.mtime
    }
}

/// 与上次记下的指纹比，磁盘上动了没有。
///
/// 四种组合都要有确定答案：`None → Some`（文件出现了）与 `Some → None`
/// （文件被删了）都算动过；两个 `None` 是**安静**的 —— 没这个文件且一直没有，
/// 不该每 500 ms 报一次「不见了」。
pub fn changed(known: Option<Stamp>, now: Option<Stamp>) -> bool {
    match (known, now) {
        (Some(before), Some(after)) => before.differs(&after),
        (None, None) => false,
        _ => true,
    }
}

/// 检测到磁盘变了之后该怎么办。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// 拿回来装进编辑器。
    Reload,
    /// **本地有未保存的改动**：一个字都不动，只提示一句。
    KeepLocal,
    /// 不是用户的文件（内置示例文档），永远不重读。
    NotOurs,
}

/// 该不该把磁盘上的那一份拿回来。
///
/// 两条规矩：
///
/// - `explicit_file`：内置示例文档的路径在临时目录里，它不是用户的文件 ——
///   外面「改了它」不可能是用户的意图（那个文件通常根本不存在）。
/// - `dirty`：编辑器里有未保存的改动时**绝不覆盖**。这是丢数据的口子：
///   用户敲了半句，外面（脚本 / 另一个编辑器 / agent）正好写了这个文件，
///   拿磁盘的版本盖上去就是把他那半句删了 —— 而且他多半不会知道。
///   提示他一句「磁盘上有新版本，没有重载」，让他自己决定存还是撤。
pub fn verdict(explicit_file: bool, dirty: bool) -> Verdict {
    if !explicit_file {
        Verdict::NotOurs
    } else if dirty {
        Verdict::KeepLocal
    } else {
        Verdict::Reload
    }
}

/// 把光标位置（行, 列；列按**字符**算，与编辑器自己的 `Position` 一致）
/// 夹进新文本里。
///
/// 外部改动挪动了字节偏移，按字节对位没有意义；按「行 / 列」至少大体停在原处
/// （改的是别的段落时就是原处）。越界一律夹到该行末尾 —— 这一处要的是
/// 「别跳回第一行、别滚到顶」，不是精确。
pub fn clamp_position(line: u32, character: u32, new_text: &str) -> (u32, u32) {
    // 按 `\n` 切而不是 `lines()`：`lines()` 会把行尾的 `\r` 吃掉，
    // 于是 CRLF 文件里算出来的列会比编辑器里的少一个。
    let line = line.min(new_text.split('\n').count().saturating_sub(1) as u32);
    let chars = new_text
        .split('\n')
        .nth(line as usize)
        .unwrap_or("")
        .chars()
        .count() as u32;
    (line, character.min(chars))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(len: u64, secs: u64) -> Stamp {
        Stamp {
            len,
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
        }
    }

    #[test]
    fn same_stamp_is_not_a_change() {
        assert!(!changed(Some(stamp(10, 5)), Some(stamp(10, 5))));
    }

    #[test]
    fn longer_or_newer_counts_as_a_change() {
        // 长度变了（内容变了）
        assert!(changed(Some(stamp(10, 5)), Some(stamp(11, 5))));
        // 长度没变、mtime 变了（原地改同样长的字）
        assert!(changed(Some(stamp(10, 5)), Some(stamp(10, 6))));
    }

    #[test]
    fn appearing_and_disappearing_both_count() {
        assert!(changed(None, Some(stamp(1, 1)))); // 文件被创建出来
        assert!(changed(Some(stamp(1, 1)), None)); // 文件被删掉
        assert!(!changed(None, None)); // 一直没有：安静（别每拍报一次）
    }

    #[test]
    fn sample_document_is_never_reloaded() {
        assert_eq!(verdict(false, false), Verdict::NotOurs);
        // 内置示例文档脏不脏都与外部改动无关：它不是用户的文件
        assert_eq!(verdict(false, true), Verdict::NotOurs);
    }

    #[test]
    fn unsaved_edits_win_over_the_disk_version() {
        assert_eq!(verdict(true, true), Verdict::KeepLocal);
    }

    #[test]
    fn clean_editor_takes_the_disk_version() {
        assert_eq!(verdict(true, false), Verdict::Reload);
    }

    #[test]
    fn position_is_clamped_into_the_new_text() {
        assert_eq!(clamp_position(2, 3, "a\nbb\nccc"), (2, 3)); // 原地不动
        assert_eq!(clamp_position(9, 0, "a\nbb\nccc"), (2, 0)); // 行越界 → 最后一行
        assert_eq!(clamp_position(1, 9, "a\nbb\nccc"), (1, 2)); // 列越界 → 该行末尾
        assert_eq!(clamp_position(3, 3, "abc"), (0, 3)); // 单行文档
    }

    #[test]
    fn position_column_counts_characters_not_bytes() {
        // 「排版中文」是 4 个字 12 个字节：列按字符算，所以是 4 不是 12
        assert_eq!(clamp_position(0, 2, "排版中文"), (0, 2));
        assert_eq!(clamp_position(0, 9, "排版中文"), (0, 4));
    }

    #[test]
    fn empty_text_clamps_everything_to_zero() {
        assert_eq!(clamp_position(5, 5, ""), (0, 0));
    }

    #[test]
    fn crlf_line_endings_line_up() {
        // CRLF 的文档：第二行是 "bb\r"，列按字符数算（3），不是被 lines() 吃掉 \r 的 2
        assert_eq!(clamp_position(1, 9, "a\r\nbb\r\nccc"), (1, 3));
    }

    #[test]
    fn missing_file_has_no_stamp() {
        assert!(Stamp::of(Path::new("no/such/dir/nope.typ")).is_none());
    }

    #[test]
    fn stamp_follows_a_real_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"hello").unwrap();
        let before = Stamp::of(file.path()).expect("刚写出来的文件该有指纹");

        std::io::Write::write_all(&mut file, b" world").unwrap();
        let after = Stamp::of(file.path()).unwrap();

        assert!(changed(Some(before), Some(after)), "写完之后该算变了");
        assert!(!changed(Some(after), Some(after)), "没动就不该算变");
    }
}

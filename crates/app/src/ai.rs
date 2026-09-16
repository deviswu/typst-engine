//! AI 编辑：把「选中的文字 + 一句要求」交给大模型，拿回可替换的 Typst 源码。
//!
//! 参考项目 `wu` 的 `src/ai.rs`，但按本项目的取舍重写了两块：
//!
//! 1. **不 shell 调外部 `typst` 拿诊断** —— 本项目编译器在进程内，
//!    「语法检查并修复」那条路的诊断该由 `typst-engine` 给（见 `AiTask::SyntaxFix`
//!    的 `errors` 参数：调用方传进来，这里不自己跑 CLI）
//! 2. **HTTP 走系统 `curl` 子进程** —— 与 `wu` 一致。理由是不引 reqwest/tokio：
//!    AI 编辑是低频操作，几十毫秒的进程启动开销无所谓，而 tokio 是几 MB 依赖
//!    （Windows 10 1803+ 自带 curl，本地模型端点同样只需要 curl）
//!
//! 提示词与解析逻辑照抄 `wu` —— 那些是调出来的，不该重写：
//! 代码围栏要先剥掉（模型很爱加解释）、SSE 要逐行解析（`-N` 关缓冲）、
//! 思考模型的 `reasoning_content` 计进度但不写进正文。

use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 默认端点（OpenAI 兼容）：DeepSeek。
const AI_BASE_URL_DEFAULT: &str = "https://api.deepseek.com/chat/completions";
/// 默认模型名。
const AI_MODEL_DEFAULT: &str = "deepseek-flash";

/// AI 编辑的上下文上限（字符）。超了按「头一半 + 省略标记 + 尾一半」截断。
pub const AI_EDIT_CONTEXT_LIMIT: usize = 12000;

/// AI 任务类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AiTask {
    /// 语法检查并修复（诊断由调用方传入）。
    SyntaxFix,
    /// 中文校对。
    Proofread,
    /// 术语一致性。
    Terminology,
    /// 中译英。
    TranslateToEnglish,
    /// 英译中。
    TranslateToChinese,
}

impl AiTask {
    pub fn label(self) -> &'static str {
        match self {
            Self::SyntaxFix => "语法检查并修复",
            Self::Proofread => "中文校对",
            Self::Terminology => "术语一致性",
            Self::TranslateToEnglish => "中译英",
            Self::TranslateToChinese => "英译中",
        }
    }
}

/// 端点：环境变量 `AI_BASE_URL` > 设置值 > 内置默认。
pub fn resolve_base_url(setting: Option<&str>) -> String {
    std::env::var("AI_BASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            setting
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| AI_BASE_URL_DEFAULT.to_string())
}

/// 模型名：环境变量 `AI_MODEL` > 设置值 > 内置默认。
pub fn resolve_model(setting: Option<&str>) -> String {
    std::env::var("AI_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            setting
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| AI_MODEL_DEFAULT.to_string())
}

/// 是不是本地端点（本地服务一般不需要 Key）。
pub fn is_local_endpoint(url: &str) -> bool {
    url.contains("127.0.0.1") || url.contains("localhost") || url.contains("0.0.0.0")
}

/// 鉴权 Key：环境变量 `AI_API_KEY` > 设置值 > `~/.pi/agent/auth.json` 里的 deepseek.key。
///
/// 最后一级只是历史兼容（`wu` 当年借用了 pi 的凭据文件）；
/// 正经用法是设环境变量或在设置文件里填 `ai_api_key`。
pub fn api_key(endpoint: &str, from_settings: Option<&str>) -> Option<String> {
    if let Ok(key) = std::env::var("AI_API_KEY")
        && !key.trim().is_empty()
    {
        return Some(key);
    }
    if let Some(key) = from_settings
        && !key.trim().is_empty()
    {
        return Some(key.trim().to_string());
    }
    if endpoint.contains("deepseek") {
        let path = home_dir()?.join(".pi").join("agent").join("auth.json");
        let text = std::fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        if let Some(key) = value.get("deepseek")?.get("key")?.as_str() {
            return Some(key.to_string());
        }
    }
    None
}

/// 用户主目录（找旧凭据文件用）。取不到就算了，不该因此失败。
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// 「选中文字 → AI 编辑」的系统提示词。
const AI_EDIT_SYSTEM: &str = "你是嵌入在 Typst 编辑器里的写作助手。用户会给你一段文档上下文和一条指令。\
请只输出可以直接放进文档的 Typst 内容本身：不要解释、不要客套、不要用代码块围栏包裹。\
需要修改时给出修改后的完整片段；需要新增时只给出新增内容。排版风格与上下文保持一致。";

/// 把「上下文 + 历史 + 本轮要求」拼成 messages。`history` 是 `(是否用户, 文本)`。
///
/// 只保留最近 6 轮：上下文无限增长既费钱又会让模型忘记最初的要求。
pub fn edit_messages(
    context: &str,
    history: &[(bool, String)],
    instruction: &str,
) -> Vec<(String, String)> {
    const HISTORY_TURNS: usize = 6;

    let mut messages = vec![
        ("system".to_string(), AI_EDIT_SYSTEM.to_string()),
        ("user".to_string(), format!("【当前文档上下文】\n{context}")),
    ];

    let start = history.len().saturating_sub(HISTORY_TURNS * 2);
    for (is_user, text) in &history[start..] {
        messages.push((
            if *is_user { "user" } else { "assistant" }.to_string(),
            text.clone(),
        ));
    }

    messages.push(("user".to_string(), instruction.to_string()));
    messages
}

/// 固定任务的提示词（语法检查 / 校对 / 术语 / 互译）。
pub fn task_prompt(task: AiTask, source: &str, errors: &str) -> String {
    let system = match task {
        AiTask::TranslateToEnglish | AiTask::TranslateToChinese => {
            "你是一位专业的中英双语翻译。严格按用户要求输出「中文在上、英文在下」的双语对照内容，\
             必须同时保留中文与英文两种语言，不得省略任何一种、不得只给一种语言；英文尽量贴近/保留原文。\
             不要解释，不要代码块。"
        }
        _ => {
            "你是一位专业的 Typst 文档写作助手。只完成指定的任务，直接从第一行开始输出修改/修复后的\
             完整 Typst 源文件本身，禁止输出任何解释、建议或多余文字，也不要包裹在代码块里。\
             非目标内容一律保持原样。"
        }
    };

    let user = match task {
        AiTask::SyntaxFix => format!(
            "下面是一个 Typst 源文件，它编译报错。请只修复导致编译错误的部分，其它内容不得改动。\n\n【编译错误】\n{errors}\n\n【源文件】\n{source}"
        ),
        AiTask::Proofread => format!(
            "下面是一个 Typst 源文件。请仅作中文校对：修正错别字、明显病句、标点与全半角错误，\
             保留原意与文档结构，不要改动 Typst 标记、公式与排版逻辑。\n\n【源文件】\n{source}"
        ),
        AiTask::Terminology => format!(
            "下面是一个 Typst 源文件。请检查并统一全文术语：把同一概念的多种叫法统一成一致写法，\
             不改动其它任何内容。\n\n【源文件】\n{source}"
        ),
        AiTask::TranslateToEnglish => format!(
            "下面是一段【中文】文本，请翻译成【英文】。以「中文在上、英文在下」的双语对照格式输出：\
             第一部分给出中文原文；第二部分给出对应的英文翻译，确保两种语言都保留。\
             英文表达地道、忠实原文、保留段落结构。只输出双语对照内容，不要任何解释。\n\n【源文本】\n{source}"
        ),
        AiTask::TranslateToChinese => format!(
            "下面是一段【英文】文本，请翻译成【中文】。以「中文在上、英文在下」的双语对照格式输出：\
             第一部分给出中文翻译；第二部分【请逐字原样保留下方这段英文原文，不要翻译回、不要改写、\
             不要省略】，紧跟在中文下面。确保输出同时包含中文和英文两种语言。\
             只输出双语对照内容，不要任何解释。\n\n【源文本】\n{source}"
        ),
    };

    format!("{system}\n\n{user}")
}

/// 超长上下文按「头一半 + 省略标记 + 尾一半」截断。返回 (文本, 是否截断过)。
pub fn truncate_context(text: &str, limit: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= limit {
        return (text.to_string(), false);
    }

    let half = limit / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(count - half).collect();
    (
        format!("{head}\n……（此处省略 {} 字）……\n{tail}", count - limit),
        true,
    )
}

/// 从模型回复里取出可直接放进文档的内容。
///
/// 模型很爱加解释和 ``` 围栏，所以先剥围栏；没有围栏就整段去两头空白。
pub fn clean_source(reply: &str) -> String {
    let text = reply.trim();

    if let Some(open) = text.find("```") {
        let after = &text[open + 3..];
        let code_start = match after.find('\n') {
            Some(i) => open + 3 + i + 1,
            None => open + 3,
        };
        if let Some(rel) = text[code_start..].find("```") {
            return text[code_start..code_start + rel].trim().to_string();
        }
        // 没有闭合围栏：退化成「跳过首行语言标签」
        if let Some(i) = after.find('\n') {
            return after[i + 1..].trim().to_string();
        }
    }

    text.to_string()
}

/// 解析一行 SSE，返回 (正文增量, 思维链增量)。非 delta 行给 `None`。
fn parse_sse_delta(line: &str) -> Option<(Option<String>, Option<String>)> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let delta = value.get("choices")?.get(0)?.get("delta")?;

    let content = delta
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let reasoning = delta
        .get("reasoning_content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());

    Some((content, reasoning))
}

/// curl 退出码 → 人话。
///
/// 直接报「退出码 7」等于没说：用户既不知道是谁的错也不知道该改什么。
/// 这几个码是 AI 场景里真会撞上的（连不上本地模型、域名写错、云端超时、证书问题）。
fn curl_hint(code: i32) -> &'static str {
    match code {
        6 => "域名解析失败（端点写错了？）",
        7 => "连不上端点（服务没起？端口/地址对不对？）",
        28 => "请求超时（网络慢，或模型想太久）",
        35 => "TLS 握手失败（代理/防火墙？）",
        60 => "证书校验失败（自签名证书？）",
        _ => "",
    }
}

/// 从**非流式**响应体里取正文（服务端忽略 `stream` 时的回退路径）。
fn parse_completion_body(bytes: &[u8]) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;

    if let Some(err) = value.get("error") {
        let message = err["message"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| err.to_string());
        return Err(format!("AI 错误：{message}"));
    }

    let text = value["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    if text.trim().is_empty() {
        return Err("AI 返回为空".to_string());
    }
    Ok(text)
}

/// 发一轮对话（OpenAI 兼容），支持取消与进度。
///
/// - `cancel` 置 true 会 kill 掉 curl 子进程
/// - `progress` 里写「已生成多少字」（含思维链 —— 思考模型的耗时主要在那儿）
pub fn chat_messages(
    base_url: &str,
    model: &str,
    key: Option<&str>,
    messages: &[(String, String)],
    cancel: &Arc<AtomicBool>,
    progress: &Arc<AtomicUsize>,
) -> Result<String, String> {
    let payload = serde_json::json!({
        "model": model,
        "messages": messages
            .iter()
            .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
            .collect::<Vec<_>>(),
        "temperature": 0.0,
        "stream": true,
    });
    let body = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

    // 请求体走临时文件（curl 的 `--data-binary @文件`），省得跟命令行长度和转义纠缠
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let body_path = std::env::temp_dir().join(format!(
        "typst_live_ai_body_{}_{nanos}.json",
        std::process::id()
    ));
    std::fs::write(&body_path, &body).map_err(|e| e.to_string())?;

    let auth = key.map(|k| format!("Authorization: Bearer {k}"));
    let data_arg = format!("@{}", body_path.to_string_lossy());

    let mut cmd = std::process::Command::new("curl");
    // `-N` 关掉缓冲，否则 SSE 会攒到最后一次性吐出来（进度就白做了）
    cmd.args(["-s", "-N", "--max-time", "120", "-X", "POST", base_url]);
    cmd.args(["-H", "Content-Type: application/json"]);
    if let Some(auth) = &auth {
        cmd.args(["-H", auth.as_str()]);
    }
    cmd.args(["--data-binary", &data_arg]);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW：别弹黑框
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = std::fs::remove_file(&body_path);
            return Err(if err.kind() == std::io::ErrorKind::NotFound {
                "没找到 curl：AI 功能需要 PATH 里有 curl（Windows 10 1803+ 自带）".to_string()
            } else {
                format!("启动 curl 失败：{err}")
            });
        }
    };

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = std::fs::remove_file(&body_path);
        return Err("拿不到 curl stdout".to_string());
    };
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = std::fs::remove_file(&body_path);
        return Err("拿不到 curl stderr".to_string());
    };

    // stdout：逐行解析 SSE，边收边记进度；同时把原始字节留着做「非流式」回退
    let progress_writer = progress.clone();
    let stdout_thread = std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        let mut raw: Vec<u8> = Vec::new();
        let mut streamed = String::new();
        let mut generated = 0usize;

        for line in reader.lines() {
            let Ok(line) = line else { break };
            raw.extend_from_slice(line.as_bytes());
            raw.push(b'\n');
            if let Some((content, reasoning)) = parse_sse_delta(&line) {
                if let Some(content) = content {
                    generated += content.chars().count();
                    streamed.push_str(&content);
                }
                // 思维链不写进正文，但计入进度
                if let Some(reasoning) = reasoning {
                    generated += reasoning.chars().count();
                }
                progress_writer.store(generated, Ordering::Relaxed);
            }
        }

        (raw, streamed)
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    // 等子进程，同时响应取消
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&body_path);
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err("已取消".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(err) => {
                let _ = std::fs::remove_file(&body_path);
                return Err(format!("等 curl 失败：{err}"));
            }
        }
    };

    let (raw, streamed) = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    let _ = std::fs::remove_file(&body_path);

    // 流式拿到了就用它；否则回退解析完整响应体
    if !streamed.trim().is_empty() {
        return Ok(streamed);
    }
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr).trim().to_string();
        let code = status.code().unwrap_or(-1);
        let hint = curl_hint(code);
        return Err(if !message.is_empty() {
            format!("AI 请求失败：{message}")
        } else if hint.is_empty() {
            format!("curl 退出码 {code}")
        } else {
            format!("{hint}（curl {code}）")
        });
    }

    parse_completion_body(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_and_models_fall_back_to_defaults() {
        // 环境变量优先，测试里不设它 → 走设置值 / 默认值
        assert_eq!(resolve_base_url(None), AI_BASE_URL_DEFAULT);
        assert_eq!(
            resolve_base_url(Some("   ")),
            AI_BASE_URL_DEFAULT,
            "空串等于没设"
        );
        assert_eq!(
            resolve_base_url(Some(" http://127.0.0.1:8080/v1 ")),
            "http://127.0.0.1:8080/v1",
            "两头的空白要去掉"
        );

        assert_eq!(resolve_model(None), AI_MODEL_DEFAULT);
        assert_eq!(resolve_model(Some("deepseek-v4-pro")), "deepseek-v4-pro");
    }

    #[test]
    fn curl_exit_codes_are_explained_in_plain_words() {
        assert!(curl_hint(7).contains("连不上"));
        assert!(curl_hint(28).contains("超时"));
        assert_eq!(curl_hint(12345), "", "没见过的码就别硬编");
    }

    #[test]
    fn local_endpoints_are_recognised() {
        assert!(is_local_endpoint(
            "http://127.0.0.1:11434/v1/chat/completions"
        ));
        assert!(is_local_endpoint("http://localhost:8080"));
        assert!(!is_local_endpoint(
            "https://api.deepseek.com/chat/completions"
        ));
    }

    #[test]
    fn truncation_keeps_head_and_tail_and_says_how_much_it_dropped() {
        let text: String = "字".repeat(100);

        let (short, truncated) = truncate_context(&text, 200);
        assert_eq!(short, text, "没超限就原样");
        assert!(!truncated);

        let (long, truncated) = truncate_context(&text, 40);
        assert!(truncated);
        assert!(long.starts_with(&"字".repeat(20)), "头 20 字要留着");
        assert!(long.ends_with(&"字".repeat(20)), "尾 20 字要留着");
        assert!(
            long.contains("此处省略 60 字"),
            "省略了多少得写出来：{long}"
        );
    }

    /// 中文按**字符**算长度，不是字节 —— 中文一个字 3 字节，按字节算会切坏。
    #[test]
    fn truncation_counts_characters_not_bytes() {
        let text = "中".repeat(30);
        let (out, truncated) = truncate_context(&text, 30);

        assert!(!truncated, "30 个汉字没超过 30 字上限");
        assert_eq!(out, text);
    }

    #[test]
    fn code_fences_are_stripped() {
        let reply = "这是说明\n```typst\n= 标题\n\n正文\n```\n后面还有话";
        assert_eq!(clean_source(reply), "= 标题\n\n正文");

        assert_eq!(clean_source("```\n= 仅围栏\n```"), "= 仅围栏");
        assert_eq!(clean_source("  裸文本  "), "裸文本", "没有围栏就只去空白");
    }

    /// 没有闭合围栏时退化成「跳过首行语言标签」，别把整段都吞了。
    #[test]
    fn an_unclosed_fence_falls_back_gracefully() {
        let reply = "```typst\n= 标题";
        assert_eq!(clean_source(reply), "= 标题");
    }

    #[test]
    fn sse_deltas_are_parsed_including_reasoning() {
        let line = r#"data: {"choices":[{"delta":{"content":"你好"}}]}"#;
        assert_eq!(
            parse_sse_delta(line),
            Some((Some("你好".to_string()), None))
        );

        let thinking = r#"data: {"choices":[{"delta":{"reasoning_content":"嗯…"}}]}"#;
        assert_eq!(
            parse_sse_delta(thinking),
            Some((None, Some("嗯…".to_string())))
        );

        assert_eq!(parse_sse_delta("data: [DONE]"), None);
        assert_eq!(parse_sse_delta("data: "), None);
        assert_eq!(parse_sse_delta("event: ping"), None, "非 data: 行不管");
        assert_eq!(parse_sse_delta("data: 不是 JSON"), None);
    }

    #[test]
    fn a_non_streaming_body_is_parsed_as_a_fallback() {
        let body = r#"{"choices":[{"message":{"content":"= 标题"}}]}"#.as_bytes();
        assert_eq!(parse_completion_body(body).unwrap(), "= 标题");

        let empty = r#"{"choices":[{"message":{"content":"  "}}]}"#.as_bytes();
        assert!(parse_completion_body(empty).is_err(), "空内容要报错");

        let err = r#"{"error":{"message":"额度不足"}}"#.as_bytes();
        let message = parse_completion_body(err).unwrap_err();
        assert!(message.contains("额度不足"), "{message}");
    }

    /// 历史只保留最近 6 轮：上下文无限增长既费钱又会让模型忘记最初要求。
    #[test]
    fn history_is_trimmed_to_the_last_six_turns() {
        let history: Vec<(bool, String)> = (0..20)
            .map(|i| (i % 2 == 0, format!("第 {i} 轮")))
            .collect();

        let messages = edit_messages("上下文", &history, "把这段改短");

        // system + 上下文 + 最近 12 条 + 本轮
        assert_eq!(messages.len(), 2 + 12 + 1);
        assert_eq!(messages[0].0, "system");
        assert!(messages[1].1.contains("上下文"));
        assert!(
            !messages.iter().any(|(_, t)| t == "第 0 轮"),
            "老历史该被丢掉"
        );
        assert!(messages.iter().any(|(_, t)| t == "第 19 轮"));
        assert_eq!(messages.last().unwrap().1, "把这段改短");
    }

    #[test]
    fn task_prompts_carry_the_task_and_the_source() {
        let prompt = task_prompt(AiTask::SyntaxFix, "= 标题", "第 3 行少个括号");
        assert!(prompt.contains("第 3 行少个括号"), "诊断要进提示词");
        assert!(prompt.contains("= 标题"));

        let prompt = task_prompt(AiTask::Proofread, "正文", "");
        assert!(prompt.contains("校对"));
        assert!(prompt.contains("正文"));
    }
}

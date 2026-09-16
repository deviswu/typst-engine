import subprocess

# ── 1) 从 HEAD 把误删的 render_ai 捞回来 ──
head = subprocess.run(
    ['git', 'show', 'HEAD:crates/app/src/main.rs'],
    capture_output=True, text=True, encoding='utf-8', cwd='D:/wuning/rust/typst-engine'
).stdout

start = head.index("    /// AI 编辑浮层：输入要求 → 生成中 → 逐块确认。")
end = head.index("    /// 底部状态栏")
render_ai = head[start:end]
print(f"从 HEAD 取回 render_ai：{render_ai.count(chr(10))} 行")

p = 'crates/app/src/main.rs'
s = open(p, encoding='utf-8').read()
assert 'fn render_ai' not in s, "render_ai 还在，别重复插"
anchor = "    /// 底部状态栏"
assert s.count(anchor) == 1
s = s.replace(anchor, render_ai + anchor)

# ── 2) 页码那段少了 `let page = ` ──
old = """                        let page_no = i + 1;
                        let current = page_no == self.current_page + 1;
                        div()"""
new = """                        let page_no = i + 1;
                        let current = page_no == self.current_page + 1;
                        let page = div()"""
assert s.count(old) == 1
s = s.replace(old, new)

# ── 3) 图片视图要走 read(cx) ──
old = """            RightPane::Image => self
                .image
                .as_ref()
                .map(|view| format!("图片 · {}", short_path(&view.path().to_string_lossy()))),"""
new = """            RightPane::Image => self
                .image
                .as_ref()
                .map(|view| format!("图片 · {}", short_path(&view.read(cx).path().to_string_lossy()))),"""
assert s.count(old) == 1
s = s.replace(old, new)

open(p, 'w', encoding='utf-8').write(s)
print("三处修好")

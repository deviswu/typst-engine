(call
  item: (ident) @function)
(call
  item: (field field: (ident) @function.method))
(tagged field: (ident) @tag)
(field field: (ident) @tag)
(comment) @comment

; CONTROL
(let "let" @keyword.storage.type)
(branch ["if" "else"] @keyword.control.conditional)
(while "while" @keyword.control.repeat)
(for ["for" "in"] @keyword.control.repeat)
(import "import" @keyword.control.import)
(as "as" @keyword.operator)
(include "include" @keyword.control.import)
(show "show" @keyword.control)
(set "set" @keyword.control)
(return "return" @keyword.control)
(flow ["break" "continue"] @keyword.control)

; OPERATOR
(in ["in" "not"] @keyword.operator)
(context "context" @keyword.control)
(and "and" @keyword.operator)
(or "or" @keyword.operator)
(not "not" @keyword.operator)
(sign ["+" "-"] @operator)
(add "+" @operator)
(sub "-" @operator)
(mul "*" @operator)
(div "/" @operator)
(cmp ["==" "<=" ">=" "!=" "<" ">"] @operator)
(fraction "/" @operator)
(fac "!" @operator)
(attach ["^" "_"] @operator)
(wildcard) @operator

; VALUE
(raw_blck "```" @operator) @markup.raw.block
(raw_span "`" @operator) @markup.raw.block
(raw_blck lang: (ident) @tag)
(label) @tag
(ref) @tag
(number) @constant.numeric
(string) @string
(content ["[" "]"] @operator)
(bool) @constant.builtin.boolean
(none) @constant.builtin
(auto) @constant.builtin
(ident) @variable

; MARKUP
(item "-" @markup.list)
(term ["/" ":"] @markup.list)
; 标题 1~5 级：整行（含 "=" 标记）统一捕获为 @title。
; 注意：gpui-component 的语法色表（SyntaxColors）只认固定的一批名字（title / keyword / type …），
; Zed 的 markup.heading.N 不在表内、查不到颜色 → 标题之前一直是默认黑字。改用 @title 后由
; theme.rs 的 apply_theme 给 title 上蓝色（浅色主题深蓝 / 深色主题亮蓝）。
; 6 级标题保持原样（不上色）。
(heading "=") @title
(heading "==") @title
(heading "===") @title
(heading "====") @title
(heading "=====") @title
(heading "======") @markup.heading.6
(url) @tag
(emph) @markup.italic
(strong) @markup.bold
(symbol) @constant.character
(shorthand) @constant.builtin
(quote) @markup.quote
(align) @operator
(letter) @constant.character
(linebreak) @constant.builtin

(math "$" @operator)
"#" @operator
"end" @operator

(escape) @constant.character.escape
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," ";" ".." ":" "sep"] @punctuation.delimiter
"assign" @punctuation
(field "." @punctuation)


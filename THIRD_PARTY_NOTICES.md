# 第三方元件聲明

本檔列出 SayItLocally fork 自行加入、且授權要求隨散布附上聲明的第三方元件。上游 SayIt 原有的依賴以各自套件內的授權為準。

## ferrous-opencc 0.4.0

- 用途：把辨識與 AI 校對的輸出轉成正體中文（台灣）。
- 作者：apoint123（<https://github.com/apoint123/ferrous-opencc>）
- 授權：Apache License 2.0，全文見 [`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt)。
- 本 fork 未修改它的原始碼，只透過 crates.io 依版本 `=0.4.0` 引用。

### 內嵌字典：Open Chinese Convert（OpenCC）

- ferrous-opencc 在編譯時內嵌 OpenCC 的轉換字典與設定（<https://github.com/BYVoid/OpenCC>）。
- 著作權：Copyright © OpenCC contributors。
- 授權：Apache License 2.0，全文同上。

### 隨同引入的依賴

ferrous-opencc-compiler（Apache-2.0）、rkyv、rkyv_derive、bytecheck、bytecheck_derive、munge、munge_macro、ptr_meta、ptr_meta_derive、rancor、rend（MIT）、fst（Unlicense 或 MIT）、hashbrown、simdutf8（MIT 或 Apache-2.0）。授權全文在各套件內。

## 只用於測試、不隨 App 散布

`tools/zh-crosstest` 是獨立的交叉測試工具，不會編進 App。它另外引用 opencc-fmmseg（MIT，字典為 OpenCC，Apache-2.0）與 zhconv（GPL-2.0-or-later，資料來自 MediaWiki 與 OpenCC）做比較。

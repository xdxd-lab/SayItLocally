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

- Apache-2.0：ferrous-opencc-compiler 0.4.0（全文同上）。
- MIT，或可選 MIT 的雙授權：下表各套件的 MIT 授權全文（含著作權聲明），逐字取自各套件隨附的授權檔，收在 [`licenses/MIT-ferrous-opencc-deps.txt`](licenses/MIT-ferrous-opencc-deps.txt)。

| 套件 | 版本 | 授權 | 著作權 |
|---|---|---|---|
| rkyv、rkyv_derive | 0.8.18 | MIT | Copyright 2021 David Koloski |
| ptr_meta、ptr_meta_derive | 0.3.2 | MIT | Copyright 2021 David Koloski |
| rend | 0.5.4 | MIT | Copyright 2021 David Koloski |
| bytecheck | 0.8.3 | MIT | Copyright 2023 David Koloski |
| bytecheck_derive | 0.8.3 | MIT | Copyright 2020 David Koloski |
| rancor | 0.1.3 | MIT | Copyright 2023 David Koloski |
| munge、munge_macro | 0.4.7 | MIT | Copyright 2024 David Koloski |
| fst | 0.4.7 | Unlicense 或 MIT（本 fork 依 MIT） | Copyright (c) 2015 Andrew Gallant |
| hashbrown | 0.17.2 | MIT 或 Apache-2.0 | Copyright (c) 2016 Amanieu d'Antras |
| simdutf8 | 0.1.5 | MIT 或 Apache-2.0 | 授權檔未寫著作權行；Cargo.toml 的作者為 Hans Kratz |

## 只用於測試、不隨 App 散布

`tools/zh-crosstest` 是獨立的交叉測試工具，不會編進 App。它另外引用 opencc-fmmseg（MIT，字典為 OpenCC，Apache-2.0）與 zhconv（GPL-2.0-or-later，資料來自 MediaWiki 與 OpenCC）做比較。

#!/usr/bin/env node
/**
 * 硬編碼中文掃描 —— i18n 的防迴歸閘門。
 *
 * 為什麼必須有：P1 是一邊翻譯一邊還在開發新功能。沒有這道閘門，
 * 翻譯永遠收不了口（翻完一片，另一片又進來新的中文串）。
 *
 * 為什麼用 Node 而不是 .ps1：這個指令碼要進 CI，也要能被任何人在任何 shell 裡跑；
 * 而且本機的 PowerShell 5.1 對 UTF-8 無 BOM 的 .ps1 會按 GBK 讀，中文註釋一亂碼
 * 就是語法錯誤（見 pitfalls 9）。Node 讀檔案永遠按 UTF-8。
 *
 * 只報**程式碼裡的**中文：註釋裡的中文有 1700 多行，是專案的正常風格，全報出來
 * 就沒人看了。所以先剝註釋，再在剩下的部分裡找。
 *
 * 用法：
 *   node scripts/check-i18n.mjs            # 只看未遷移的檔案清單（摘要）
 *   node scripts/check-i18n.mjs --all      # 逐行列出
 *   node scripts/check-i18n.mjs --strict   # 白名單外只要有一處就退出碼 1（CI 用）
 */
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join, relative, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const SRC_ROOT = fileURLToPath(new URL('../src', import.meta.url))
const CLIENT_ROOT = fileURLToPath(new URL('..', import.meta.url))
const CJK = /[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\u3000-\u303f\uff01-\uff5e]/

/**
 * 允許含中文的路徑（字首匹配，相對 client/）。
 *
 * 加白名單前先問一句：這裡的中文是**給使用者看的文案**，還是**被處理的資料**？
 * 是文案就該進 locale 檔案，不該進這個列表。
 */
const ALLOWLIST = [
  // locale 文件本身就是中文的家
  'src/i18n/locales/',
]

/**
 * 測試整體不掃。
 *
 * 測試裡的中文分兩種，兩種都不該由這個指令碼管：中文文本處理的**被測資料**
 * （textPostProcess / textReplacement / textSegmenter）翻掉等於廢掉測試；
 * 中文的 describe/it 標題是專案既有風格，也不是使用者可見文案。
 * 真正要處理的是「斷言了使用者可見文案」的那幾個測試 —— 它們由 i18n-todo.md 的
 * P1「每片都要遵守的三條」逐片改成斷言 key，不靠這裡掃。
 */
const SKIP_DIRS = ['__tests__']

const args = new Set(process.argv.slice(2))
const showAll = args.has('--all')
const strict = args.has('--strict')

/**
 * 少量中文是演算法輸入、錯誤匹配詞或 Prompt，不是介面文案。它們必須就地標明邊界：
 *   // i18n-allow-start: 原因
 *   ...
 *   // i18n-allow-end
 * 或在單行末尾寫 `// i18n-allow: 原因`。
 *
 * 不使用整檔案白名單：同一個檔案以後新增 UI 文案時，閘門仍然必須能攔住。
 */
function collectAllowedLines(file, source) {
  const allowed = new Set()
  let inAllowedBlock = false
  source.split('\n').forEach((line, index) => {
    const lineNumber = index + 1
    if (line.includes('i18n-allow-start')) {
      if (inAllowedBlock) throw new Error(`${file}:${lineNumber}: nested i18n allow block`)
      inAllowedBlock = true
    }
    if (inAllowedBlock || line.includes('i18n-allow:')) allowed.add(lineNumber)
    if (line.includes('i18n-allow-end')) {
      if (!inAllowedBlock) throw new Error(`${file}:${lineNumber}: unmatched i18n-allow-end`)
      inAllowedBlock = false
    }
  })
  if (inAllowedBlock) throw new Error(`${file}: unclosed i18n allow block`)
  return allowed
}

function isTextBearingNode(node) {
  return ts.isStringLiteralLike(node)
    || ts.isRegularExpressionLiteral(node)
    || ts.isJsxText(node)
    || node.kind === ts.SyntaxKind.TemplateHead
    || node.kind === ts.SyntaxKind.TemplateMiddle
    || node.kind === ts.SyntaxKind.TemplateTail
}

/** 用 TypeScript AST 找字串、模板、正則與 JSX 文本；註釋天然不會進入結果。 */
function findChinese(file, source) {
  const scriptKind = file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS
  const sourceFile = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, scriptKind)
  const sourceLines = source.split('\n')
  const allowedLines = collectAllowedLines(file, source)
  const results = new Map()

  const visit = (node) => {
    if (isTextBearingNode(node)) {
      const nodeText = node.getText(sourceFile)
      const isJsxComment = ts.isJsxText(node) && /^\s*\{\/\*/.test(nodeText)
      if (!isJsxComment && CJK.test(nodeText)) {
        const start = sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line
        nodeText.split('\n').forEach((part, offset) => {
          const line = start + offset
          const lineNumber = line + 1
          if (!allowedLines.has(lineNumber) && CJK.test(part)) {
            results.set(lineNumber, { line: lineNumber, text: (sourceLines[line] ?? '').trim() })
          }
        })
      }
    }
    if (!ts.isJsxText(node)) {
      ts.forEachChild(node, visit)
    }
  }
  visit(sourceFile)
  return [...results.values()]
}

function walk(dir, files = []) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    if (statSync(full).isDirectory()) {
      if (!SKIP_DIRS.includes(entry)) walk(full, files)
    } else if (/\.(ts|tsx)$/.test(entry)) {
      files.push(full)
    }
  }
  return files
}

function isAllowed(relPath) {
  const posix = relPath.split(sep).join('/')
  return ALLOWLIST.some((prefix) => posix.startsWith(prefix))
}

const findings = []
for (const file of walk(SRC_ROOT)) {
  const relPath = relative(CLIENT_ROOT, file)
  if (isAllowed(relPath)) continue
  const source = readFileSync(file, 'utf8')
  for (const item of findChinese(file, source)) {
    findings.push({ file: relPath.split(sep).join('/'), ...item })
  }
}

if (findings.length === 0) {
  console.log('check-i18n: no hardcoded Chinese found outside the allowlist.')
  process.exit(0)
}

const byFile = new Map()
for (const item of findings) {
  byFile.set(item.file, (byFile.get(item.file) ?? 0) + 1)
}

if (showAll) {
  for (const item of findings) {
    console.log(`${item.file}:${item.line}: ${item.text}`)
  }
} else {
  for (const [file, count] of [...byFile].sort((a, b) => b[1] - a[1])) {
    console.log(`${String(count).padStart(4)}  ${file}`)
  }
}

console.log(`\ncheck-i18n: ${findings.length} line(s) in ${byFile.size} file(s) still hold Chinese in code.`)
if (strict) {
  console.error('check-i18n: --strict is on, failing.')
  process.exit(1)
}
process.exit(0)

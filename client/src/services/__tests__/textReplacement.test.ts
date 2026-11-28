import { describe, it, expect, vi, beforeEach } from 'vitest'

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

import {
  applyReplacements,
  parseBatchReplacements,
  toTaiwanReplacementTargets,
  type TextReplacementRule,
} from '../textReplacement'

function rule(from: string, to: string, enabled = true): TextReplacementRule {
  return { id: '1', from, to, enabled }
}

describe('applyReplacements', () => {
  it('替換匹配的文本', () => {
    const rules = [rule('你好', 'Hello')]
    expect(applyReplacements('你好世界', rules)).toBe('Hello世界')
  })

  it('替換多次出現', () => {
    const rules = [rule('啊', '')]
    expect(applyReplacements('啊這個啊那個啊', rules)).toBe('這個那個')
  })

  it('停用的規則不生效', () => {
    const rules = [rule('你好', 'Hello', false)]
    expect(applyReplacements('你好世界', rules)).toBe('你好世界')
  })

  it('空 from 不替換', () => {
    const rules = [rule('', 'Hello')]
    expect(applyReplacements('你好世界', rules)).toBe('你好世界')
  })

  it('多條規則按順序執行', () => {
    const rules = [
      rule('A', 'B'),
      rule('B', 'C'),
    ]
    // A → B → C（鏈式替換）
    expect(applyReplacements('A', rules)).toBe('C')
  })

  it('空規則列表返回原文', () => {
    expect(applyReplacements('你好', [])).toBe('你好')
  })

  it('空文本返回空', () => {
    expect(applyReplacements('', [rule('a', 'b')])).toBe('')
  })
})

describe('parseBatchReplacements', () => {
  it('英文逗號分隔', () => {
    expect(parseBatchReplacements('安卓說話,按住說話')).toEqual([
      { from: '安卓說話', to: '按住說話' },
    ])
  })

  it('中文逗號分隔', () => {
    expect(parseBatchReplacements('安卓說話，按住說話')).toEqual([
      { from: '安卓說話', to: '按住說話' },
    ])
  })

  it('製表符分隔（從表格貼上）', () => {
    expect(parseBatchReplacements('Cloud Code\tClaude Code')).toEqual([
      { from: 'Cloud Code', to: 'Claude Code' },
    ])
  })

  it('=> 與 -> 分隔', () => {
    expect(parseBatchReplacements('a => b\nc -> d')).toEqual([
      { from: 'a', to: 'b' },
      { from: 'c', to: 'd' },
    ])
  })

  it('多行、忽略空行、去除首尾空白', () => {
    const input = ' 原文1, 替換1 \n\n原文2,替換2\n'
    expect(parseBatchReplacements(input)).toEqual([
      { from: '原文1', to: '替換1' },
      { from: '原文2', to: '替換2' },
    ])
  })

  it('無分隔符的行：替換為留空（刪除）', () => {
    expect(parseBatchReplacements('嗯')).toEqual([{ from: '嗯', to: '' }])
  })

  it('取最靠前的分隔符，替換內容可含其它逗號', () => {
    expect(parseBatchReplacements('abc,a, b')).toEqual([{ from: 'abc', to: 'a, b' }])
  })

  it('原文為空的行忽略', () => {
    expect(parseBatchReplacements(',只有替換')).toEqual([])
  })
})

describe('applyReplacements 的順序語義', () => {
  const rule = (id: string, from: string, to: string) => ({ id, from, to, enabled: true })

  it('順序即執行順序：前一條的結果會參與後一條的匹配（級聯）', () => {
    // A: 甲 → 乙，B: 乙 → 丙。A 在前時會一路級聯成"丙"
    const cascade = applyReplacements('甲', [rule('a', '甲', '乙'), rule('b', '乙', '丙')])
    expect(cascade).toBe('丙')

    // 換成 B 在前，"乙 → 丙"先跑（此時還沒有"乙"），結果停在"乙"
    const noCascade = applyReplacements('甲', [rule('b', '乙', '丙'), rule('a', '甲', '乙')])
    expect(noCascade).toBe('乙')
  })

  it('兩條規則匹配同一段文本時，靠前的先生效', () => {
    const first = applyReplacements('安卓說話', [
      rule('a', '安卓說話', '按住說話'),
      rule('b', '安卓', 'Android'),
    ])
    expect(first).toBe('按住說話')

    const second = applyReplacements('安卓說話', [
      rule('b', '安卓', 'Android'),
      rule('a', '安卓說話', '按住說話'),
    ])
    expect(second).toBe('Android說話')
  })

  it('停用的規則不參與，也不影響其餘規則的順序', () => {
    const result = applyReplacements('甲', [
      { ...rule('a', '甲', '乙'), enabled: false },
      rule('b', '甲', '丙'),
    ])
    expect(result).toBe('丙')
  })
})

describe('toTaiwanReplacementTargets', () => {
  beforeEach(() => invoke.mockReset())

  it('有漢字時交給 Rust 轉換器，回傳轉換後的值', async () => {
    invoke.mockResolvedValue(['軟體', 'ok'])
    // 「軟體」的簡體寫法
    await expect(toTaiwanReplacementTargets(['\u8f6f\u4ef6', 'ok'])).resolves.toEqual(['軟體', 'ok'])
    expect(invoke).toHaveBeenCalledWith('zh_to_taiwan_texts', { texts: ['\u8f6f\u4ef6', 'ok'] })
  })

  it('沒有漢字時不呼叫', async () => {
    await expect(toTaiwanReplacementTargets(['Claude Code', ''])).resolves.toEqual(['Claude Code', ''])
    expect(invoke).not.toHaveBeenCalled()
  })

  it('轉換失敗或結果長度不符時保留原文', async () => {
    invoke.mockRejectedValueOnce(new Error('boom'))
    await expect(toTaiwanReplacementTargets(['\u8f6f\u4ef6'])).resolves.toEqual(['\u8f6f\u4ef6'])
    invoke.mockResolvedValueOnce([])
    await expect(toTaiwanReplacementTargets(['\u8f6f\u4ef6'])).resolves.toEqual(['\u8f6f\u4ef6'])
  })
})

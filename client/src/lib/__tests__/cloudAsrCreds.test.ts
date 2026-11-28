import { describe, it, expect } from 'vitest'
import {
  describeDoubaoMissing,
  doubaoKeyLabel,
  effectiveDoubaoCredentials,
  resolveDoubaoConsole,
  type DoubaoCredentials,
} from '../cloudAsrCreds'

function creds(over: Partial<DoubaoCredentials> = {}): DoubaoCredentials {
  return { console: 'new', consoleKey: '', accessToken: '', appId: '', ...over }
}

describe('resolveDoubaoConsole', () => {
  it('存過就用存的值', () => {
    expect(resolveDoubaoConsole('new', '1234567890')).toBe('new')
    expect(resolveDoubaoConsole('legacy', '')).toBe('legacy')
  })

  it('沒存過但有 App ID = 老使用者，判為舊版，行為不變', () => {
    expect(resolveDoubaoConsole(null, '1234567890')).toBe('legacy')
    expect(resolveDoubaoConsole(undefined, '1234567890')).toBe('legacy')
    expect(resolveDoubaoConsole('', '1234567890')).toBe('legacy')
  })

  it('全新使用者預設新版', () => {
    expect(resolveDoubaoConsole(null, '')).toBe('new')
    expect(resolveDoubaoConsole('', '   ')).toBe('new')
  })

  it('存了無法識別的值時按 App ID 兜底，不崩', () => {
    expect(resolveDoubaoConsole('garbage', '123')).toBe('legacy')
    expect(resolveDoubaoConsole('garbage', '')).toBe('new')
  })
})

describe('effectiveDoubaoCredentials', () => {
  // 這條是整個改動的核心不變數：Rust 側靠 app_id 空不空來選鑑權頭。
  it('新版模式必須把 appId 清空，否則會用舊版格式發新版金鑰', () => {
    const r = effectiveDoubaoCredentials(
      creds({ console: 'new', consoleKey: 'APPKEY', appId: '1234567890', accessToken: 'TOKEN' }),
    )
    expect(r).toEqual({ apiKey: 'APPKEY', appId: '' })
  })

  it('舊版模式用 Access Token + App ID', () => {
    const r = effectiveDoubaoCredentials(
      creds({ console: 'legacy', consoleKey: 'APPKEY', accessToken: 'TOKEN', appId: '1234567890' }),
    )
    expect(r).toEqual({ apiKey: 'TOKEN', appId: '1234567890' })
  })

  it('兩代金鑰互不串用', () => {
    const both = creds({ consoleKey: 'NEWKEY', accessToken: 'OLDTOKEN', appId: '123' })
    expect(effectiveDoubaoCredentials({ ...both, console: 'new' }).apiKey).toBe('NEWKEY')
    expect(effectiveDoubaoCredentials({ ...both, console: 'legacy' }).apiKey).toBe('OLDTOKEN')
  })

  it('去掉貼上時常帶進來的前後空白', () => {
    expect(effectiveDoubaoCredentials(creds({ console: 'new', consoleKey: '  K  ' })))
      .toEqual({ apiKey: 'K', appId: '' })
    expect(
      effectiveDoubaoCredentials(
        creds({ console: 'legacy', accessToken: ' T ', appId: ' 123 ' }),
      ),
    ).toEqual({ apiKey: 'T', appId: '123' })
  })
})

describe('describeDoubaoMissing', () => {
  it('新版只需要 API Key', () => {
    expect(describeDoubaoMissing(creds({ console: 'new' }))).toBe('還沒填 API Key')
    expect(describeDoubaoMissing(creds({ console: 'new', consoleKey: 'K' }))).toBe('')
  })

  it('新版不因為缺 App ID 而攔人', () => {
    expect(describeDoubaoMissing(creds({ console: 'new', consoleKey: 'K', appId: '' }))).toBe('')
  })

  it('舊版兩個都要，且先報金鑰', () => {
    expect(describeDoubaoMissing(creds({ console: 'legacy' }))).toBe('還沒填 Access Token')
    expect(describeDoubaoMissing(creds({ console: 'legacy', accessToken: 'T' })))
      .toBe('還沒填 App ID')
    expect(describeDoubaoMissing(creds({ console: 'legacy', accessToken: 'T', appId: '123' })))
      .toBe('')
  })
})

describe('doubaoKeyLabel', () => {
  it('兩代叫法不同，標錯使用者會在控制台找不到', () => {
    expect(doubaoKeyLabel('new')).toBe('API Key')
    expect(doubaoKeyLabel('legacy')).toBe('Access Token')
  })
})

import { describe, it, expect } from 'vitest'
import { describeWebDavError, webDavErrorDetail } from '../webdavBackup'

// 真實出現過的原文（堅果雲斷網時的 MKCOL）
const NETWORK = 'MKCOL failed: error sending request for url (https://dav.jianguoyun.com/dav)'

describe('describeWebDavError', () => {
  it('連不上伺服器的 reqwest 原文翻成一句話，帶上主機名', () => {
    const text = describeWebDavError(NETWORK)
    expect(text).toContain('dav.jianguoyun.com')
    expect(text).not.toContain('error sending request')
  })

  it('已登記的錯誤碼照舊翻譯', () => {
    expect(describeWebDavError('WEBDAV_UNAUTHORIZED')).not.toBe('WEBDAV_UNAUTHORIZED')
  })

  it('沒認出來的錯誤按原樣顯示', () => {
    expect(describeWebDavError('PUT failed: HTTP 507')).toBe('PUT failed: HTTP 507')
  })
})

describe('webDavErrorDetail', () => {
  it('翻譯過的錯誤保留原文給「詳情」', () => {
    expect(webDavErrorDetail(NETWORK)).toBe(NETWORK)
  })

  it('原樣顯示的錯誤不再重複一份詳情', () => {
    expect(webDavErrorDetail('PUT failed: HTTP 507')).toBeNull()
  })
})

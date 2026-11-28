import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(), emit: vi.fn() }))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }))

import { framedBody } from '../bridge'

/** 佈局必須與 Rust 的 ipc_body::split_framed_bytes 一致：[4 位元組小端 JSON 長度][JSON][載荷]。 */
describe('framedBody', () => {
  it('前 4 字节是小端的 JSON 字节长度，后面依次是 JSON 和原始载荷', () => {
    const meta = { modelId: 'm', hotwords: ['中文热词'] }
    const body = framedBody(meta, new Uint8Array([9, 8, 7]))
    const jsonLen = new DataView(body.buffer).getUint32(0, true)
    const json = new TextDecoder().decode(body.slice(4, 4 + jsonLen))
    expect(JSON.parse(json)).toEqual(meta)
    expect(Array.from(body.slice(4 + jsonLen))).toEqual([9, 8, 7])
    // 中文按 UTF-8 計位元組，不是按字元
    expect(jsonLen).toBe(new TextEncoder().encode(JSON.stringify(meta)).length)
  })
})

import { describe, expect, it, vi } from 'vitest'
import { storeDelete, storeGet, storeSet } from './bridge'

const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

describe('setting IPC ordering', () => {
  it('keeps unawaited writes, reads and deletes in order for the same key', async () => {
    const values = new Map<string, unknown>()
    let release!: () => void
    const blocked = new Promise<void>(resolve => { release = resolve })
    invoke.mockImplementation(async (command: string, { key, value }: { key: string; value?: unknown }) => {
      if (command === 'store_set' && value === 1) await blocked
      if (command === 'store_set') values.set(key, value)
      if (command === 'store_delete') values.delete(key)
      return values.get(key)
    })

    const first = storeSet('ordered', 1)
    const second = storeSet('ordered', 2)
    const read = storeGet('ordered')
    const remove = storeDelete('ordered')
    const after = storeGet('ordered')
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['store_set'])
    release()

    await Promise.all([first, second, remove])
    await expect(read).resolves.toBe(2)
    await expect(after).resolves.toBeUndefined()
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      'store_set', 'store_set', 'store_get', 'store_delete', 'store_get',
    ])
  })

  it('continues processing the same key after an IPC error', async () => {
    invoke.mockReset()
      .mockRejectedValueOnce(new Error('write failed'))
      .mockResolvedValueOnce('prior value')
    const write = storeSet('retry', true)
    const read = storeGet('retry')
    await expect(write).rejects.toThrow('write failed')
    await expect(read).resolves.toBe('prior value')
  })
})

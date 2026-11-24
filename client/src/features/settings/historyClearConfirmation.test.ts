import { describe, expect, it } from 'vitest'
import { canConfirmHistoryClear, HISTORY_CLEAR_DELAY_MS } from './historyClearConfirmation'

describe('second history clear confirmation', () => {
  const openedAt = 100

  it('rejects another click until the full delay has passed', () => {
    const readyAt = openedAt + HISTORY_CLEAR_DELAY_MS
    expect(canConfirmHistoryClear(openedAt, readyAt, 1)).toBe(false)
    expect(canConfirmHistoryClear(openedAt + 599, readyAt, 1)).toBe(false)
    expect(canConfirmHistoryClear(openedAt + 600, readyAt, 1)).toBe(true)
  })

  it('rejects double-clicks after the delay and allows keyboard activation', () => {
    const readyAt = openedAt + HISTORY_CLEAR_DELAY_MS
    expect(canConfirmHistoryClear(readyAt + 1, readyAt, 2)).toBe(false)
    expect(canConfirmHistoryClear(readyAt + 1, readyAt, 0)).toBe(true)
  })
})

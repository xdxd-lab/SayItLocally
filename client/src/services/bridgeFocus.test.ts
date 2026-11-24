import { expect, it, vi } from 'vitest'
import { pasteText, releaseRecordingFocus } from './bridge'

const invoke = vi.hoisted(() => vi.fn(async () => {}))
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

it('forwards the original focus token to native paste and release', async () => {
  await pasteText('dictated text', '20', '20', false, 'run-focus')
  expect(invoke).toHaveBeenCalledWith('paste_text', {
    text: 'dictated text',
    hwnd: '20',
    focusHwnd: '20',
    restoreClipboard: false,
    focusToken: 'run-focus',
  })
  await releaseRecordingFocus('run-focus')
  expect(invoke).toHaveBeenCalledWith('release_recording_focus', { focusToken: 'run-focus' })
})

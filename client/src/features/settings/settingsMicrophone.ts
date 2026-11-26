import { isPseudoInputDevice, listMicrophones, microphoneOpened, normalizeSelectedMicId, type MicEndpoint } from '@/services/audio'
import { getSetting, setSetting } from '@/services/store'
import { drawBars, resetWaveform } from '@/services/waveform'
import type { TranslationKey } from '@/i18n'

export type MicVolumeLevel = 'idle' | 'silent' | 'low' | 'normal'

export function createSettingsMicrophone({
  onDevices,
  onTesting,
  onRevealing,
  onVolume,
  onError,
  canvas,
}: {
  onDevices: (devices: MicEndpoint[]) => void
  onTesting: (testing: boolean) => void
  onRevealing: (revealing: boolean) => void
  onVolume: (volume: MicVolumeLevel) => void
  onError: (error: TranslationKey | null) => void
  canvas: () => HTMLCanvasElement | null
}) {
  const media = navigator.mediaDevices
  let disposed = false
  let devices: MicEndpoint[] = []
  let revision = 0
  // Revision of the enumeration that decided the current device set.
  let applied = 0
  let closeActive: (() => void) | null = null

  function reportError(error: unknown) {
    const name = error instanceof Error ? error.name : ''
    onError(name === 'NotAllowedError' ? 'mic.error.denied'
      : name === 'NotFoundError' ? 'mic.error.notFound' : 'mic.error.failed')
  }

  // `named` supplies labels by device id; it never adds or removes devices.
  function updateDevices(next: MicEndpoint[], named: MicEndpoint[] = []) {
    const labels = new Map([...devices, ...named].filter((device) => device.deviceId && device.label.trim())
      .map((device) => [device.deviceId, device.label]))
    devices = next.map((device) => {
      const label = labels.get(device.deviceId)
      return !device.label.trim() && label
        ? { deviceId: device.deviceId, groupId: device.groupId, label }
        : device
    })
    onDevices(devices)
  }

  async function refresh() {
    const id = ++revision
    try {
      const next = await listMicrophones()
      if (!disposed && id > applied) {
        applied = id
        updateDevices(next)
      }
    } catch {
      // Passive refresh must not request access or clear a working list.
    }
  }

  const refreshDevices = () => { void refresh() }
  media?.addEventListener('devicechange', refreshDevices)
  microphoneOpened.addEventListener('change', refreshDevices)
  refreshDevices()

  async function start(mode: 'names' | 'test', deviceId = '') {
    if (disposed || closeActive) return
    let stream: MediaStream | undefined
    let context: AudioContext | undefined
    let frame: number | undefined
    let interval: ReturnType<typeof setInterval> | undefined
    let timeout: ReturnType<typeof setTimeout> | undefined
    const setBusy = mode === 'names' ? onRevealing : onTesting

    const close = () => {
      if (closeActive !== close) return
      closeActive = null
      clearTimeout(timeout)
      clearInterval(interval)
      if (frame !== undefined) cancelAnimationFrame(frame)
      stream?.getTracks().forEach((track) => track.stop())
      if (context) void context.close().catch(() => {})
      if (!disposed) setBusy(false)
    }

    closeActive = close
    setBusy(true)
    onError(null)
    if (mode === 'test') onVolume('idle')

    try {
      const opened = await media.getUserMedia({ audio: deviceId ? { deviceId: { exact: deviceId } } : true })
      if (closeActive !== close) {
        opened.getTracks().forEach((track) => track.stop())
        return
      }
      stream = opened
      if (mode === 'names') {
        // Some browsers expose names only during capture. Stop as soon as the
        // list arrives (or if the single enumeration stalls), without sampling audio.
        timeout = setTimeout(() => {
          close()
          onError('mic.error.namesUnavailable')
        }, 1000)
        const id = ++revision
        try {
          const next = await listMicrophones()
          if (closeActive === close) {
            // [自訂] SC-55：較晚開始的列舉若已決定了裝置清單，這次只補名稱，不把已拔除的裝置加回。
            // 沒有權限時的匿名清單看不出是哪些裝置，仍以這次有名稱的結果為準。
            if (id > applied || devices.some((device) => !device.deviceId)) {
              applied = Math.max(applied, id)
              updateDevices(next)
            } else {
              updateDevices(devices, next)
            }
            if (!next.some((device) => !isPseudoInputDevice(device.deviceId) && device.label.trim())) {
              onError('mic.error.namesUnavailable')
            }
          }
        } catch {
          if (closeActive === close) onError('mic.error.namesUnavailable')
        } finally {
          close()
        }
        return
      }

      void refresh()
      context = new AudioContext()
      const analyser = context.createAnalyser()
      analyser.fftSize = 256
      analyser.smoothingTimeConstant = 0.7
      context.createMediaStreamSource(stream).connect(analyser)
      resetWaveform()

      const draw = () => {
        if (closeActive !== close) return
        const element = canvas()
        const surface = element?.getContext('2d')
        if (element && surface) drawBars(surface, analyser, element.width, element.height)
        frame = requestAnimationFrame(draw)
      }
      frame = requestAnimationFrame(draw)

      const samples = new Float32Array(analyser.frequencyBinCount)
      let peakRms = 0
      let hasSignal = false
      interval = setInterval(() => {
        if (closeActive !== close) return
        analyser.getFloatTimeDomainData(samples)
        let sum = 0
        for (const sample of samples) {
          sum += sample * sample
          hasSignal ||= sample !== 0
        }
        peakRms = Math.max(peakRms, Math.sqrt(sum / samples.length))
        onVolume(!hasSignal ? 'silent' : peakRms < 0.02 ? 'low' : 'normal')
      }, 500)
      timeout = setTimeout(close, 5000)
    } catch (error) {
      if (closeActive !== close) return
      close()
      reportError(error)
    }
  }

  return {
    revealNames: () => start('names'),
    test: (deviceId: string) => start('test', deviceId),
    dispose() {
      disposed = true
      media?.removeEventListener('devicechange', refreshDevices)
      microphoneOpened.removeEventListener('change', refreshDevices)
      closeActive?.()
    },
  }
}

export type SettingsMicrophone = ReturnType<typeof createSettingsMicrophone> & {
  select: (id: string) => Promise<void>
}

// Loading and refreshing are read-only. Only a selection made by the user writes the setting.
export function mountSettingsMicrophone(
  ref: { current: SettingsMicrophone | null },
  { onSelectedMic, ...callbacks }: Parameters<typeof createSettingsMicrophone>[0] & {
    onSelectedMic: (id: string) => void
  },
) {
  let disposed = false
  let selected = false
  const mic = createSettingsMicrophone(callbacks)
  ref.current = {
    ...mic,
    async select(id) {
      selected = true
      onSelectedMic(id)
      await setSetting('selectedMic', id)
    },
  }
  void getSetting('selectedMic', '').then((raw) => {
    if (!disposed && !selected) onSelectedMic(normalizeSelectedMicId(raw))
  }).catch(() => {})
  return () => {
    disposed = true
    mic.dispose()
    ref.current = null
  }
}

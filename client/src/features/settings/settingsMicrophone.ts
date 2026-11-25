import { listMicrophones, microphoneOpened } from '@/services/audio'
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
  onDevices: (devices: MediaDeviceInfo[]) => void
  onTesting: (testing: boolean) => void
  onRevealing: (revealing: boolean) => void
  onVolume: (volume: MicVolumeLevel) => void
  onError: (error: TranslationKey | null) => void
  canvas: () => HTMLCanvasElement | null
}) {
  const media = navigator.mediaDevices
  let disposed = false
  let denied = false
  let revision = 0
  let closeActive: (() => void) | null = null

  function reportError(error: unknown) {
    const name = error instanceof Error ? error.name : ''
    denied ||= name === 'NotAllowedError'
    onError(name === 'NotAllowedError' ? 'mic.error.denied'
      : name === 'NotFoundError' ? 'mic.error.notFound' : 'mic.error.failed')
  }

  async function refresh() {
    const id = ++revision
    try {
      const devices = await listMicrophones()
      if (!disposed && id === revision) onDevices(devices)
    } catch {
      // Passive refresh must not request access or clear a working list.
    }
  }

  const refreshDevices = () => { void refresh() }
  function refreshAfterRecording() {
    if (disposed) return
    if (denied) {
      denied = false
      onError(null)
    }
    refreshDevices()
  }

  media?.addEventListener('devicechange', refreshDevices)
  microphoneOpened.addEventListener('change', refreshAfterRecording)
  refreshDevices()

  async function start(mode: 'names' | 'test', deviceId = '') {
    if (disposed || denied || closeActive) return
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
          revision += 1
          close()
          onError('mic.error.failed')
        }, 1000)
        await refresh()
        close()
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
      microphoneOpened.removeEventListener('change', refreshAfterRecording)
      closeActive?.()
    },
  }
}

// The mock backend's picture models: the downloads, and one tiny PNG from
// whatever the image card has saved. Nothing here is a real model.

import type { GeneratedImage, LocalModelInfo } from './types'

const imageModels: LocalModelInfo[] = [
  {
    id: 'sdCli',
    name: 'Image engine',
    description: 'stable-diffusion.cpp, which runs the picture models on this computer.',
    languages: '—',
    bytes: 27_322_680,
    installed: false,
    progress: null,
    error: null,
  },
  {
    id: 'flux2KleinQ4',
    name: 'FLUX.2 Klein',
    description: 'Quantized FLUX.2 Klein 4B. Makes a picture in four steps.',
    languages: 'English',
    bytes: 4_976_410_684,
    installed: false,
    progress: null,
    error: null,
  },
  {
    id: 'zImageTurboQ4',
    name: 'Z-Image Turbo',
    description: 'Quantized Z-Image Turbo. Makes a picture in eight steps.',
    languages: 'English and Chinese',
    bytes: 6_696_835_812,
    installed: false,
    progress: null,
    error: null,
  },
]

const downloadTimers = new Map<string, ReturnType<typeof setInterval>>()

export function mockImageModels(): LocalModelInfo[] {
  return imageModels.map((m) => ({ ...m }))
}

export function mockDownloadImageModel(id: string): void {
  const model = imageModels.find((m) => m.id === id)
  if (!model || model.installed || downloadTimers.has(id)) return
  model.error = null
  model.progress = { done: 0, total: model.bytes }
  const timer = setInterval(() => {
    if (!model.progress) return
    model.progress = {
      done: Math.min(model.bytes, model.progress.done + model.bytes / 8),
      total: model.bytes,
    }
    if (model.progress.done >= model.bytes) {
      clearInterval(timer)
      downloadTimers.delete(id)
      model.installed = true
      model.progress = null
    }
  }, 300)
  downloadTimers.set(id, timer)
}

export function mockCancelImageModelDownload(id: string): void {
  const timer = downloadTimers.get(id)
  if (timer) clearInterval(timer)
  downloadTimers.delete(id)
  const model = imageModels.find((m) => m.id === id)
  if (model) model.progress = null
}

export function mockDeleteImageModel(id: string): void {
  const model = imageModels.find((m) => m.id === id)
  if (!model) return
  mockCancelImageModelDownload(id)
  model.installed = false
}

/** A 1×1 PNG, so the card can show that a picture came back. */
export function mockGenerateImage(): GeneratedImage {
  return {
    png: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  }
}

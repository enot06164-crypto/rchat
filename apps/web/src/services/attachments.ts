import { createSignal } from "solid-js";
import type { AttachmentKind, AttachmentUpload, Message } from "../types/rchat";
import { chatApi } from "./api";

/**
 * Медиапайплайн клиента (по образцу Telegram):
 *
 * - фото сжимается: ресайз до 1280px по большей стороне + JPEG;
 *   режим «без сжатия» отправляет исходные байты;
 * - для видео генерируется постер-кадр (отдельный блоб, как thumbnail);
 * - для голосового считаются пики звуковой волны (для отрисовки в UI).
 *
 * Содержимое уходит в ядро зашифрованным (сервер ключа не знает).
 */

/** Кэш «data URL» по ключу сообщения (local_id или server_id). */
const cache = new Map<string, string>();

/** Кэш «идёт загрузка», чтобы не дублировать запросы. */
const pending = new Map<string, Promise<string | null>>();

export function rememberAttachment(key: string, dataUrl: string): void {
  cache.set(key, dataUrl);
}

export function cachedAttachment(key: string): string | undefined {
  return cache.get(key);
}

/** Ключ кэша сообщения: server_id стабильнее local_id (у оптимистичного — null). */
function cacheKey(message: Message): string {
  return message.server_id !== null
    ? `s:${message.server_id}`
    : `l:${message.local_id}`;
}

/**
 * Data URL содержимого вложения сообщения (или `null`, если вложения нет /
 * загрузка не удалась).
 */
export async function attachmentUrl(
  message: Message
): Promise<string | null> {
  if (message.attachment === null) return null;
  const key = cacheKey(message);
  const cached = cache.get(key);
  if (cached !== undefined) return cached;
  if (message.server_id === null) return null;
  const existing = pending.get(key);
  if (existing !== undefined) return existing;

  const request = chatApi
    .downloadAttachment(message.server_id)
    .then((data) => {
      const url = `data:${data.mime || "application/octet-stream"};base64,${data.data_base64}`;
      cache.set(key, url);
      return url;
    })
    .catch(() => null)
    .finally(() => {
      pending.delete(key);
    });
  pending.set(key, request);
  return request;
}

/** Data URL постера видео (отдельный блоб). */
export async function attachmentThumbUrl(
  message: Message
): Promise<string | null> {
  if (message.attachment?.thumb == null) return null;
  if (message.server_id === null) return null;
  const key = `t:${message.server_id}`;
  const cached = cache.get(key);
  if (cached !== undefined) return cached;
  try {
    const data = await chatApi.downloadAttachment(message.server_id);
    if (data.thumb_base64 == null) return null;
    const url = `data:${message.attachment.thumb.mime};base64,${data.thumb_base64}`;
    cache.set(key, url);
    return url;
  } catch {
    return null;
  }
}

/** Сигнал содержимого вложения для конкретного сообщения. */
export function useAttachmentUrl(message: () => Message) {
  const [url, setUrl] = createSignal<string | null>(null);
  const load = async (): Promise<void> => {
    setUrl(await attachmentUrl(message()));
  };
  return { url, load };
}

/** Определяет вид вложения по MIME-типу и имени файла. */
export function detectAttachmentKind(file: File): AttachmentKind {
  const mime = file.type.toLowerCase();
  if (mime.startsWith("image/")) return "photo";
  if (mime.startsWith("video/")) return "video";
  if (mime.startsWith("audio/")) return "voice";
  const name = file.name.toLowerCase();
  if (/\.(png|jpe?g|gif|webp|bmp|heic|avif)$/.test(name)) return "photo";
  if (/\.(mp4|mov|webm|mkv|m4v)$/.test(name)) return "video";
  if (/\.(ogg|oga|mp3|m4a|wav|opus|webm)$/.test(name)) return "voice";
  return "file";
}

/** Байты файла в base64. */
export async function toBase64(file: Blob): Promise<string> {
  const buffer = await file.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

/** Размер файла из base64 (без декодирования). */
function base64Size(base64: string): number {
  const padding = base64.endsWith("==") ? 2 : base64.endsWith("=") ? 1 : 0;
  return Math.floor((base64.length * 3) / 4) - padding;
}

/** Готовит вложение к отправке: читает файл и кодирует в base64. */
export async function prepareAttachment(file: File): Promise<AttachmentUpload> {
  const data_base64 = await toBase64(file);
  return {
    kind: detectAttachmentKind(file),
    name: file.name === "" ? "file" : file.name,
    mime: file.type === "" ? "application/octet-stream" : file.type,
    size: file.size,
    data_base64,
  };
}

/** Человекочитаемый размер файла. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} Б`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} КБ`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} МБ`;
}

// ---------------------------------------------------------------------------
// Фото: сжатие (как в Telegram) и постер
// ---------------------------------------------------------------------------

/** Предел стороны фото после сжатия (как в Telegram — 1280px). */
const PHOTO_MAX_SIDE = 1280;
/** Предел стороны постера. */
const THUMB_MAX_SIDE = 320;

/** Читает изображение в canvas с ресайзом по большей стороне. */
async function drawScaled(
  source: CanvasImageSource,
  width: number,
  height: number,
  maxSide: number
): Promise<Blob | null> {
  const scale = Math.min(1, maxSide / Math.max(width, height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(width * scale));
  canvas.height = Math.max(1, Math.round(height * scale));
  const context = canvas.getContext("2d");
  if (context === null) return null;
  context.drawImage(source, 0, 0, canvas.width, canvas.height);
  return new Promise((resolve) => {
    canvas.toBlob(
      (blob) => resolve(blob),
      "image/jpeg",
      0.85
    );
  });
}

/** Загружает файл как изображение. */
function loadImage(file: Blob): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const image = new Image();
    image.onload = () => {
      URL.revokeObjectURL(url);
      resolve(image);
    };
    image.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("не удалось прочитать изображение"));
    };
    image.src = url;
  });
}

/**
 * Подготовка фото: сжатое изображение (по умолчанию) или исходный файл
 * («без сжатия») + постер-превью отдельным вложением.
 */
export async function preparePhoto(
  file: File,
  compress: boolean
): Promise<{
  attachment: AttachmentUpload;
  thumb: AttachmentUpload | null;
}> {
  const kind = detectAttachmentKind(file);
  let main: Blob = file;
  let mainName = file.name === "" ? "photo.jpg" : file.name;
  let mainMime = file.type === "" ? "image/jpeg" : file.type;

  if (compress) {
    try {
      const image = await loadImage(file);
      const blob = await drawScaled(
        image,
        image.naturalWidth,
        image.naturalHeight,
        PHOTO_MAX_SIDE
      );
      if (blob !== null) {
        main = blob;
        mainName = file.name.replace(/\.[^.]+$/, "") + ".jpg";
        mainMime = "image/jpeg";
      }
    } catch {
      // Не смогли прочитать — отправляем как есть.
    }
  }

  // Постер — маленькая копия (отдельный блоб, как thumbnail в Telegram).
  let thumb: AttachmentUpload | null = null;
  try {
    const image = await loadImage(file);
    const blob = await drawScaled(
      image,
      image.naturalWidth,
      image.naturalHeight,
      THUMB_MAX_SIDE
    );
    if (blob !== null) {
      thumb = {
        kind: "photo",
        name: "thumb.jpg",
        mime: "image/jpeg",
        size: blob.size,
        data_base64: await toBase64(blob),
      };
    }
  } catch {
    thumb = null;
  }

  const data_base64 = await toBase64(main);
  return {
    attachment: {
      kind,
      name: mainName,
      mime: mainMime,
      size: base64Size(data_base64),
      data_base64,
    },
    thumb,
  };
}

// ---------------------------------------------------------------------------
// Видео: постер первого кадра
// ---------------------------------------------------------------------------

/** Генерирует постер-кадр видео (первый доступный кадр). */
export async function prepareVideoPoster(
  file: File
): Promise<AttachmentUpload | null> {
  try {
    const url = URL.createObjectURL(file);
    const video = document.createElement("video");
    video.muted = true;
    video.playsInline = true;
    video.preload = "metadata";
    video.src = url;
    await new Promise<void>((resolve, reject) => {
      video.onloadeddata = () => resolve();
      video.onerror = () => reject(new Error("видео не читается"));
    });
    // Немного прокручиваем, чтобы попасть в первый содержательный кадр.
    video.currentTime = Math.min(0.5, (video.duration || 1) / 10);
    await new Promise<void>((resolve) => {
      video.onseeked = () => resolve();
      setTimeout(() => resolve(), 1500);
    });
    const blob = await drawScaled(
      video,
      video.videoWidth || 320,
      video.videoHeight || 240,
      THUMB_MAX_SIDE
    );
    URL.revokeObjectURL(url);
    if (blob === null) return null;
    return {
      kind: "photo",
      name: "poster.jpg",
      mime: "image/jpeg",
      size: blob.size,
      data_base64: await toBase64(blob),
    };
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Голосовое: пики звуковой волны
// ---------------------------------------------------------------------------

/** Число пиков волны (как полоска в Telegram). */
const WAVEFORM_BUCKETS = 48;

/**
 * Считает пики звуковой волны записи (0..255 на бакет) — по ним UI рисует
 * полоску, как в Telegram.
 */
export async function computeWaveform(blob: Blob): Promise<number[]> {
  try {
    const buffer = await blob.arrayBuffer();
    const context = new AudioContext();
    const audio = await context.decodeAudioData(buffer.slice(0));
    const channel = audio.getChannelData(0);
    const bucket = Math.max(1, Math.floor(channel.length / WAVEFORM_BUCKETS));
    const peaks: number[] = [];
    for (let i = 0; i < WAVEFORM_BUCKETS; i++) {
      let peak = 0;
      const start = i * bucket;
      const end = Math.min(channel.length, start + bucket);
      for (let j = start; j < end; j += 16) {
        const value = Math.abs(channel[j]);
        if (value > peak) peak = value;
      }
      peaks.push(Math.min(255, Math.round(peak * 255)));
    }
    await context.close();
    return peaks;
  } catch {
    return [];
  }
}

/** Пики волны в base64 (для отправки в сообщении). */
export function waveformToBase64(peaks: number[]): string {
  if (peaks.length === 0) return "";
  const bytes = new Uint8Array(peaks.length);
  peaks.forEach((value, index) => {
    bytes[index] = Math.max(0, Math.min(255, Math.round(value)));
  });
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

import { createSignal } from "solid-js";
import type { AttachmentKind, AttachmentUpload, Message } from "../types/rchat";
import { chatApi } from "./api";

/**
 * Содержимое вложений в веб-клиенте.
 *
 * Байты лежат в ядре (в зашифрованном payload сообщения) — клиент тянет их
 * отдельной командой `download_attachment` и кэширует как data URL, чтобы не
 * качать одно и то же повторно. Для оптимистичного сообщения содержимое уже
 * есть на руках, поэтому оно кладётся в кэш сразу.
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

/** Готовит вложение к отправке: читает файл и кодирует в base64. */
export async function prepareAttachment(file: File): Promise<AttachmentUpload> {
  const buffer = await file.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return {
    kind: detectAttachmentKind(file),
    name: file.name === "" ? "file" : file.name,
    mime: file.type === "" ? "application/octet-stream" : file.type,
    size: file.size,
    data_base64: btoa(binary),
  };
}

/** Человекочитаемый размер файла. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} Б`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} КБ`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} МБ`;
}

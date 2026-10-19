/*
 * rchat C ABI (tdjson-подобный JSON-интерфейс).
 *
 * Один RchatHandle — один независимый клиент/профиль (свой каталог данных,
 * SQLite, секреты и E2E-состояние). Схема запросов, ответов и событий
 * описана в docs/ffi.md.
 *
 * Все возвращённые строки — UTF-8 JSON без встроенных NUL. Строки от
 * rchat_send_json/rchat_receive_json освобождаются rchat_free_string().
 * Строки rchat_version()/rchat_last_error() заимствованные, их нельзя
 * освобождать.
 *
 * Потоки: handle можно использовать из разных потоков, но rchat_send_json
 * блокирует вызывающий поток до ответа. rchat_destroy нельзя вызывать
 * одновременно с другими операциями над тем же handle.
 */
#ifndef RCHAT_H
#define RCHAT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct RchatHandle RchatHandle;

/* Создаёт клиент по JSON-конфигурации. При ошибке возвращает NULL;
 * причина — rchat_last_error(). */
RchatHandle *rchat_create(const char *config_json);

/* Останавливает клиент и освобождает handle. NULL допустим. */
void rchat_destroy(RchatHandle *handle);

/* Синхронно выполняет JSON-запрос и возвращает JSON-ответ. */
char *rchat_send_json(RchatHandle *handle, const char *request_json);

/* Забирает событие с ожиданием до timeout_ms (0 — без ожидания).
 * NULL означает тайм-аут или пустую очередь. */
char *rchat_receive_json(RchatHandle *handle, uint64_t timeout_ms);

/* Освобождает строку, полученную из send/receive. NULL допустим. */
void rchat_free_string(char *value);

/* Версия библиотеки (заимствованная строка). */
const char *rchat_version(void);

/* Версия JSON-схемы. */
uint32_t rchat_api_version(void);

/* Последняя ошибка rchat_create на вызывающем потоке (заимствованная
 * строка) или NULL. */
const char *rchat_last_error(void);

#ifdef __cplusplus
}
#endif

#endif /* RCHAT_H */

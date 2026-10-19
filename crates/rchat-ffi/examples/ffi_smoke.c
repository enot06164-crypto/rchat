/*
 * Минимальный smoke-пример C-ABI rchat.
 *
 * Сборка (Linux, из корня workspace):
 *   cargo build -p rchat-ffi
 *   cc -std=c11 -Wall -Wextra \
 *      -I crates/rchat-ffi/include \
 *      crates/rchat-ffi/examples/ffi_smoke.c \
 *      -L target/debug -lrchat_ffi -o /tmp/rchat-ffi-smoke
 *   LD_LIBRARY_PATH=target/debug /tmp/rchat-ffi-smoke
 *
 * Пример не подключается к серверу: auto_start=false, команда ping работает
 * локально. Для реального сценария нужен запущенный rchat-server.
 */

#include <stdio.h>
#include <string.h>

#include "rchat.h"

int main(void) {
    const char *config =
        "{\"data_dir\":\"/tmp/rchat-ffi-smoke-data\","
        "\"server\":\"ws://127.0.0.1:8443\","
        "\"auto_start\":false}";

    printf("rchat-ffi %s (schema %u)\n", rchat_version(),
           (unsigned)rchat_api_version());

    RchatHandle *handle = rchat_create(config);
    if (handle == NULL) {
        const char *error = rchat_last_error();
        fprintf(stderr, "rchat_create failed: %s\n", error ? error : "unknown");
        return 1;
    }

    char *response = rchat_send_json(handle, "{\"@type\":\"ping\"}");
    if (response == NULL) {
        fprintf(stderr, "rchat_send_json returned NULL\n");
        rchat_destroy(handle);
        return 1;
    }

    printf("%s\n", response);
    int ok = strstr(response, "\"@type\":\"ok\"") != NULL;
    rchat_free_string(response);
    rchat_destroy(handle);
    return ok ? 0 : 1;
}

/* Test-only Linux fault injection, confined to the child CLI and its temporary store. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int fsync(int fd) {
    int (*real_fsync)(int) = dlsym(RTLD_NEXT, "fsync");
    const char *base = getenv("MEM04_FAULT_BASE");
    const char *stage = getenv("MEM04_FAULT_STAGE");
    char link[64], path[4096], registry[4096];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t count = readlink(link, path, sizeof(path) - 1);
    if (base && stage && count >= 0) {
        path[count] = '\0';
        snprintf(registry, sizeof(registry), "%s/.projects", base);
        if ((strcmp(stage, "parent") == 0 && strcmp(path, base) == 0) ||
            (strcmp(stage, "published") == 0 && strcmp(path, registry) == 0)) {
            errno = EIO;
            return -1;
        }
    }
    if (!real_fsync) { errno = EIO; return -1; }
    return real_fsync(fd);
}

/* Independent C-compiler view of fs_ntfs_dirent_t and of one directory
 * listing, printed as key=value and tab-separated lines for
 * tests/python/test_dir_listing.py to compare against the ctypes client.
 * Usage: abi_dir_listing [image path] */
#include <stddef.h>
#include <stdio.h>
#include "fs_ntfs.h"

#define OFF(f) printf("offset.%s=%zu\n", #f, offsetof(fs_ntfs_dirent_t, f))

int main(int argc, char **argv) {
    printf("sizeof=%zu\n", sizeof(fs_ntfs_dirent_t));
    OFF(file_record_number); OFF(file_type); OFF(name_len); OFF(name);
    if (argc < 3) return 0;
    fs_ntfs_fs_t *fs = fs_ntfs_mount(argv[1]);
    if (!fs) { fprintf(stderr, "mount: %s\n", fs_ntfs_last_error()); return 2; }
    fs_ntfs_dir_iter_t *it = fs_ntfs_dir_open(fs, argv[2]);
    if (!it) {
        printf("open_errno=%d\n", fs_ntfs_last_errno());
        fs_ntfs_umount(fs);
        return 0;
    }
    const fs_ntfs_dirent_t *d;
    while ((d = fs_ntfs_dir_next(it)) != NULL)
        printf("entry\t%llu\t%u\t%u\t%s\n", (unsigned long long)d->file_record_number,
               d->file_type, d->name_len, d->name);
    printf("end_errno=%d\n", fs_ntfs_last_errno());
    printf("skipped=%lld\n", (long long)fs_ntfs_dir_skipped(it));
    fs_ntfs_dir_close(it);
    fs_ntfs_umount(fs);
    return 0;
}

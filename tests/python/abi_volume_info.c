/* Independent C-compiler view of fs_ntfs_volume_info_v2_t and of one
 * fs_ntfs_get_volume_info_v2 call, printed as key=value lines for
 * tests/python/test_volume_info.py to compare against the ctypes client. */
#include <stddef.h>
#include <stdio.h>
#include "fs_ntfs.h"

#define OFF(f) printf("offset.%s=%zu\n", #f, offsetof(fs_ntfs_volume_info_v2_t, f))

int main(int argc, char **argv) {
    printf("sizeof=%zu\n", sizeof(fs_ntfs_volume_info_v2_t));
    OFF(volume_name); OFF(cluster_size); OFF(total_clusters);
    OFF(ntfs_version_major); OFF(ntfs_version_minor); OFF(serial_number);
    OFF(total_size); OFF(volume_flags); OFF(is_dirty); OFF(_pad);
    OFF(mft_record_size); OFF(bytes_per_sector);
    if (argc < 2) return 0;
    fs_ntfs_fs_t *fs = fs_ntfs_mount(argv[1]);
    if (!fs) { fprintf(stderr, "mount: %s\n", fs_ntfs_last_error()); return 2; }
    fs_ntfs_volume_info_v2_t vi;
    int rc = fs_ntfs_get_volume_info_v2(fs, &vi);
    if (rc != 0) { fprintf(stderr, "info: %s\n", fs_ntfs_last_error()); return 3; }
    printf("volume_name=%s\ncluster_size=%u\ntotal_clusters=%llu\n",
           vi.volume_name, vi.cluster_size, (unsigned long long)vi.total_clusters);
    printf("ntfs_version_major=%u\nntfs_version_minor=%u\nserial_number=%llu\n",
           vi.ntfs_version_major, vi.ntfs_version_minor,
           (unsigned long long)vi.serial_number);
    printf("total_size=%llu\nvolume_flags=%u\nis_dirty=%u\n",
           (unsigned long long)vi.total_size, vi.volume_flags, vi.is_dirty);
    printf("mft_record_size=%u\nbytes_per_sector=%u\n",
           vi.mft_record_size, vi.bytes_per_sector);
    fs_ntfs_umount(fs);
    return 0;
}

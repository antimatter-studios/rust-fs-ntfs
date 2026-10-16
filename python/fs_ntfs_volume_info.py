"""Read-only ctypes client for fs_ntfs_get_volume_info_v2.

The library path is always explicit: nothing here searches the system for a
shared library. Build one with

    cargo rustc --release --lib --crate-type cdylib

which leaves the crate's declared staticlib and rlib outputs untouched.
Only volume information and directory listing reads are exposed; there is
no write, format or administration API in this module.
"""

import ctypes
import os


class FsNtfsVolumeInfoV2(ctypes.Structure):
    """Mirror of fs_ntfs_volume_info_v2_t in include/fs_ntfs.h."""

    _fields_ = [
        ("volume_name", ctypes.c_char * 128),
        ("cluster_size", ctypes.c_uint32),
        ("total_clusters", ctypes.c_uint64),
        ("ntfs_version_major", ctypes.c_uint16),
        ("ntfs_version_minor", ctypes.c_uint16),
        ("serial_number", ctypes.c_uint64),
        ("total_size", ctypes.c_uint64),
        ("volume_flags", ctypes.c_uint16),
        ("is_dirty", ctypes.c_uint8),
        ("_pad", ctypes.c_uint8 * 5),
        ("mft_record_size", ctypes.c_uint32),
        ("bytes_per_sector", ctypes.c_uint32),
    ]


DIRENT_NAME_BYTES = 1024


class FsNtfsDirent(ctypes.Structure):
    """Mirror of fs_ntfs_dirent_t in include/fs_ntfs.h."""

    _fields_ = [
        ("file_record_number", ctypes.c_uint64),
        ("file_type", ctypes.c_uint8),
        ("name_len", ctypes.c_uint16),
        ("name", ctypes.c_char * DIRENT_NAME_BYTES),
    ]


class NtfsError(OSError):
    """A C ABI call failed; errno and message come from the library."""


class ClosedVolumeError(ValueError):
    """The volume was used after close()."""


def load_library(path):
    """Load the shared library at an explicit path and declare signatures."""
    if not path or not os.path.isabs(path):
        raise ValueError("an absolute shared-library path is required")
    lib = ctypes.CDLL(path)
    lib.fs_ntfs_mount.argtypes = [ctypes.c_char_p]
    lib.fs_ntfs_mount.restype = ctypes.c_void_p
    lib.fs_ntfs_umount.argtypes = [ctypes.c_void_p]
    lib.fs_ntfs_umount.restype = None
    lib.fs_ntfs_get_volume_info_v2.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(FsNtfsVolumeInfoV2),
    ]
    lib.fs_ntfs_get_volume_info_v2.restype = ctypes.c_int
    lib.fs_ntfs_dir_open.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.fs_ntfs_dir_open.restype = ctypes.c_void_p
    lib.fs_ntfs_dir_next.argtypes = [ctypes.c_void_p]
    lib.fs_ntfs_dir_next.restype = ctypes.POINTER(FsNtfsDirent)
    lib.fs_ntfs_dir_skipped.argtypes = [ctypes.c_void_p]
    lib.fs_ntfs_dir_skipped.restype = ctypes.c_int64
    lib.fs_ntfs_dir_close.argtypes = [ctypes.c_void_p]
    lib.fs_ntfs_dir_close.restype = None
    lib.fs_ntfs_last_errno.argtypes = []
    lib.fs_ntfs_last_errno.restype = ctypes.c_int
    lib.fs_ntfs_last_error.argtypes = []
    lib.fs_ntfs_last_error.restype = ctypes.c_char_p
    return lib


def _error(lib, what):
    code = lib.fs_ntfs_last_errno()
    raw = lib.fs_ntfs_last_error()
    message = raw.decode("utf-8", "replace") if raw else ""
    return NtfsError(code, f"{what}: {message}")


class Volume:
    """A read-only mount; use as a context manager. close() is idempotent."""

    def __init__(self, lib, image_path):
        encoded = os.fsencode(image_path)
        if b"\0" in encoded:
            raise ValueError("image path contains an embedded NUL")
        self._lib = lib
        self._handle = lib.fs_ntfs_mount(encoded)
        if not self._handle:
            raise _error(lib, "fs_ntfs_mount")

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    @property
    def closed(self):
        return self._handle is None

    def close(self):
        if self._handle is not None:
            for iterator in list(self._iterators):
                iterator.close()
            handle, self._handle = self._handle, None
            self._lib.fs_ntfs_umount(handle)

    _iterators = ()

    def open_dir(self, path):
        """Open a read-only directory iterator; it closes before the volume."""
        if self._handle is None:
            raise ClosedVolumeError("volume is closed")
        iterator = DirIterator(self, path)
        if not isinstance(self._iterators, set):
            self._iterators = set()
        self._iterators.add(iterator)
        return iterator

    def list_dir(self, path):
        """Every entry of one directory, or IncompleteListingError.

        A listing with skipped index entries is never returned as complete."""
        with self.open_dir(path) as iterator:
            entries = list(iterator)
            skipped = iterator.skipped()
        if skipped:
            raise IncompleteListingError(path, skipped, entries)
        return entries

    def volume_info(self):
        if self._handle is None:
            raise ClosedVolumeError("volume is closed")
        info = FsNtfsVolumeInfoV2()
        rc = self._lib.fs_ntfs_get_volume_info_v2(self._handle, ctypes.byref(info))
        if rc != 0:
            raise _error(self._lib, "fs_ntfs_get_volume_info_v2")
        return {
            "volume_name": info.volume_name.decode("utf-8"),
            "cluster_size": info.cluster_size,
            "total_clusters": info.total_clusters,
            "ntfs_version": (info.ntfs_version_major, info.ntfs_version_minor),
            "serial_number": info.serial_number,
            "total_size": info.total_size,
            "volume_flags": info.volume_flags,
            "is_dirty": bool(info.is_dirty),
            "mft_record_size": info.mft_record_size,
            "bytes_per_sector": info.bytes_per_sector,
        }


class IncompleteListingError(RuntimeError):
    """fs_ntfs_dir_skipped reported index entries the listing does not hold."""

    def __init__(self, path, skipped, entries):
        super().__init__(f"{path!r}: {skipped} index entries were skipped")
        self.path = path
        self.skipped = skipped
        self.entries = entries


class DirEntry:
    """A copy of one borrowed fs_ntfs_dirent_t, safe after next() or close().

    file_record_number is the MFT record the $I30 index entry names, without
    its sequence number; file_type is the duplicated type stored in that
    index entry, which the header documents can be stale."""

    __slots__ = ("file_record_number", "file_type", "name")

    def __init__(self, name, file_record_number, file_type):
        self.name = name
        self.file_record_number = file_record_number
        self.file_type = file_type

    def __repr__(self):
        return (
            f"DirEntry(name={self.name!r}, file_record_number="
            f"{self.file_record_number}, file_type={self.file_type})"
        )


def _copy_entry(raw):
    length = raw.name_len
    if length >= DIRENT_NAME_BYTES:
        raise NtfsError(0, f"dirent name_len {length} exceeds the name buffer")
    data = ctypes.string_at(ctypes.addressof(raw) + FsNtfsDirent.name.offset, length)
    if b"\0" in data:
        raise NtfsError(0, "dirent name contains an embedded NUL")
    return DirEntry(data.decode("utf-8"), raw.file_record_number, raw.file_type)


class DirIterator:
    """Iterates one directory; use as a context manager. close() is idempotent.

    Entries are copied out of the library's borrowed buffer before the next
    call. A NULL from fs_ntfs_dir_next with errno 0 ends the listing; any
    other errno raises NtfsError. skipped() must be consulted before a
    listing is treated as complete; Volume.list_dir does that."""

    def __init__(self, volume, path):
        encoded = os.fsencode(path)
        if b"\0" in encoded:
            raise ValueError("directory path contains an embedded NUL")
        self._volume = volume
        self._lib = volume._lib
        self._handle = self._lib.fs_ntfs_dir_open(volume._handle, encoded)
        if not self._handle:
            raise _error(self._lib, "fs_ntfs_dir_open")
        self._exhausted = False

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def __iter__(self):
        return self

    @property
    def closed(self):
        return self._handle is None

    def _require_open(self):
        if self._handle is None:
            raise ClosedVolumeError("directory iterator is closed")

    def __next__(self):
        self._require_open()
        if self._exhausted:
            raise StopIteration
        raw = self._lib.fs_ntfs_dir_next(self._handle)
        if not raw:
            code = self._lib.fs_ntfs_last_errno()
            if code != 0:
                raise _error(self._lib, "fs_ntfs_dir_next")
            self._exhausted = True
            raise StopIteration
        return _copy_entry(raw.contents)

    def skipped(self):
        self._require_open()
        count = self._lib.fs_ntfs_dir_skipped(self._handle)
        if count < 0:
            raise _error(self._lib, "fs_ntfs_dir_skipped")
        return count

    def close(self):
        if self._handle is not None:
            handle, self._handle = self._handle, None
            self._lib.fs_ntfs_dir_close(handle)
            iterators = self._volume._iterators
            if isinstance(iterators, set):
                iterators.discard(self)

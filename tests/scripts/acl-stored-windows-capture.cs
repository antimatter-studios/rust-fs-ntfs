// Test-only capture, independent of the Rust reader. Windows supplies file IDs
// and MFT records; the detached fixed VHD supplies the actual $SDS bytes.
// API layouts:
// https://learn.microsoft.com/windows/win32/api/winioctl/ni-winioctl-fsctl_get_ntfs_file_record
// https://learn.microsoft.com/windows/win32/api/winioctl/ns-winioctl-ntfs_volume_data_buffer
// https://learn.microsoft.com/windows/win32/api/fileapi/ns-fileapi-by_handle_file_information
using System;
using System.IO;
using System.Text;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace AclOracle {
    public sealed class StoredCapture {
        public ulong FileReference;
        public uint SecurityId;
        public uint ClusterSize;
        public byte[] FileRecord;
        public byte[] SecureRecord;
        public byte[] Descriptor;
        public long SdsOffset = -1;

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
            IntPtr security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern bool GetFileInformationByHandle(SafeFileHandle file, [Out] byte[] info);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern bool DeviceIoControl(SafeFileHandle file, uint code,
            byte[] input, int inputSize, [Out] byte[] output, int outputSize,
            out int returned, IntPtr overlapped);

        [StructLayout(LayoutKind.Sequential, Pack = 4)]
        struct TokenPrivilege { public int Count; public long Luid; public int Attributes; }
        [DllImport("advapi32.dll", SetLastError = true)]
        static extern bool OpenProcessToken(IntPtr process, int access, out IntPtr token);
        [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern bool LookupPrivilegeValueW(string system, string name, out long luid);
        [DllImport("advapi32.dll", SetLastError = true)]
        static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll,
            ref TokenPrivilege state, int length, IntPtr previous, IntPtr returned);
        [DllImport("kernel32.dll")]
        static extern IntPtr GetCurrentProcess();

        // CreateFileW always asks for SYNCHRONIZE and FILE_READ_ATTRIBUTES. An
        // empty DACL grants neither, so the file is opened for backup instead:
        // SeBackupPrivilege grants read access regardless of the DACL, which
        // stays exactly as Windows stored it.
        // https://learn.microsoft.com/windows/win32/fileio/file-security-and-access-rights
        static void EnableBackupPrivilege() {
            IntPtr token;
            // TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY
            if (!OpenProcessToken(GetCurrentProcess(), 0x28, out token))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcessToken");
            var state = new TokenPrivilege { Count = 1, Attributes = 2 }; // SE_PRIVILEGE_ENABLED
            if (!LookupPrivilegeValueW(null, "SeBackupPrivilege", out state.Luid))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "LookupPrivilegeValueW");
            if (!AdjustTokenPrivileges(token, false, ref state, 0, IntPtr.Zero, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "AdjustTokenPrivileges");
            // ERROR_NOT_ALL_ASSIGNED: the token does not hold the privilege.
            int status = Marshal.GetLastWin32Error();
            if (status != 0) throw new Win32Exception(status, "SeBackupPrivilege");
        }
        static SafeFileHandle Open(string path, uint access, uint flags = 0) {
            var handle = CreateFileW(path, access, 7, IntPtr.Zero, 3, flags, IntPtr.Zero);
            if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), path);
            return handle;
        }
        static byte[] Ioctl(SafeFileHandle volume, uint code, byte[] input, int size) {
            var output = new byte[size]; int count;
            if (!DeviceIoControl(volume, code, input, input == null ? 0 : input.Length,
                output, output.Length, out count, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "NTFS capture ioctl");
            Array.Resize(ref output, count);
            return output;
        }
        static ushort U16(byte[] b, int at) { return BitConverter.ToUInt16(b, at); }
        static uint U32(byte[] b, int at) { return BitConverter.ToUInt32(b, at); }
        static byte[] Slice(byte[] b, int at, int length) {
            var result = new byte[length]; Buffer.BlockCopy(b, at, result, 0, length); return result;
        }
        static byte[] Record(SafeFileHandle volume, ulong reference, int size) {
            var output = Ioctl(volume, 0x90068, BitConverter.GetBytes(reference), size + 16);
            if ((BitConverter.ToUInt64(output, 0) & 0xffffffffffffUL) != (reference & 0xffffffffffffUL))
                throw new IOException("Windows returned a different MFT record");
            int length = checked((int)U32(output, 8));
            var record = Slice(output, 12, length);
            if (Encoding.ASCII.GetString(record, 0, 4) != "FILE") throw new IOException("not FILE");
            return record;
        }
        static byte[] Attribute(byte[] record, uint type, string name) {
            // Depending on the Windows buffer representation, transfer fixups
            // may still be present. Keep the captured buffer intact as evidence.
            record = (byte[])record.Clone();
            int usa = U16(record, 4), count = U16(record, 6);
            if (count > 1 && record.Length % (count - 1) == 0) {
                int sector = record.Length / (count - 1);
                for (int i = 1; i < count; i++) {
                    int end = i * sector - 2;
                    if (U16(record, end) == U16(record, usa))
                        Buffer.BlockCopy(record, usa + i * 2, record, end, 2);
                }
            }
            int used = checked((int)U32(record, 24));
            for (int at = U16(record, 20); at + 16 <= used;) {
                uint current = U32(record, at);
                if (current == 0xffffffff) break;
                int length = checked((int)U32(record, at + 4));
                if (length < 24 || at + length > used) throw new IOException("invalid attribute length");
                string currentName = Encoding.Unicode.GetString(record,
                    at + U16(record, at + 10), record[at + 9] * 2);
                if (current == type && currentName == name) return Slice(record, at, length);
                at += length;
            }
            return null;
        }
        public static StoredCapture Capture(string file) {
            var result = new StoredCapture();
            EnableBackupPrivilege();
            // FILE_FLAG_BACKUP_SEMANTICS
            using (var handle = Open(file, 0, 0x02000000)) {
                var info = new byte[52];
                if (!GetFileInformationByHandle(handle, info)) throw new Win32Exception(Marshal.GetLastWin32Error());
                result.FileReference = ((ulong)U32(info, 44) << 32) | U32(info, 48);
            }
            using (var volume = Open(@"\\.\" + file.Substring(0, 2), 0x80000000)) {
                var geometry = Ioctl(volume, 0x90064, null, 128);
                result.ClusterSize = U32(geometry, 44);
                int size = checked((int)U32(geometry, 48));
                result.FileRecord = Record(volume, result.FileReference, size);
                result.SecureRecord = Record(volume, 9, size);
            }
            var own = Attribute(result.FileRecord, 0x50, "");
            if (own != null) {
                if (own[8] != 0) throw new IOException("nonresident own descriptor needs capture support");
                result.Descriptor = Slice(own, U16(own, 20), checked((int)U32(own, 16)));
            } else {
                var standard = Attribute(result.FileRecord, 0x10, "");
                if (standard == null || standard[8] != 0 || U32(standard, 16) < 56)
                    throw new IOException("missing NTFS 3 standard information");
                result.SecurityId = U32(standard, U16(standard, 20) + 52);
            }
            return result;
        }
        public void ReadDetached(string image, long partitionOffset) {
            if (Descriptor != null) return;
            var data = Attribute(SecureRecord, 0x80, "$SDS");
            if (data == null || data[8] == 0 || BitConverter.ToInt64(data, 16) != 0)
                throw new IOException("expected base nonresident $SDS attribute");
            long length = BitConverter.ToInt64(data, 48);
            if (length <= 0 || length > 16 * 1024 * 1024) throw new IOException("unexpected $SDS size");
            var stream = new byte[checked((int)length)];
            long lcn = 0, destination = 0;
            using (var imageFile = File.OpenRead(image)) {
                for (int at = U16(data, 32); at < data.Length && data[at] != 0;) {
                    int header = data[at++], n = header & 15, m = header >> 4;
                    if (n < 1 || n > 8 || m > 8 || at + n + m > data.Length)
                        throw new IOException("invalid $SDS run");
                    ulong clusters = 0;
                    for (int i = 0; i < n; i++) clusters |= (ulong)data[at++] << (8 * i);
                    long delta = m > 0 && (data[at + m - 1] & 128) != 0 ? -1L : 0L;
                    for (int i = m - 1; i >= 0; i--) delta = (delta << 8) | data[at + i];
                    at += m; lcn = checked(lcn + delta);
                    long runBytes = checked((long)clusters * ClusterSize);
                    if (lcn < 0 || runBytes <= 0) throw new IOException("invalid $SDS run");
                    int count = checked((int)Math.Min(runBytes, length - destination));
                    if (m == 0) { destination += count; if (destination == length) break; continue; }
                    imageFile.Position = checked(partitionOffset + lcn * ClusterSize);
                    int done = 0;
                    while (done < count) {
                        int got = imageFile.Read(stream, checked((int)destination) + done, count - done);
                        if (got == 0) throw new EndOfStreamException(); done += got;
                    }
                    destination += count;
                    if (destination == length) break;
                }
            }
            if (destination != length) throw new IOException("incomplete $SDS capture");
            for (int at = 0; at + 20 <= stream.Length;) {
                int size = checked((int)U32(stream, at + 16));
                if (size >= 20 && size <= stream.Length - at && U32(stream, at + 4) == SecurityId) {
                    SdsOffset = at; Descriptor = Slice(stream, at + 20, size - 20); return;
                }
                at = size >= 20 && size <= stream.Length - at ? (at + size + 15) & ~15 : at + 16;
            }
            throw new IOException("security ID missing from raw $SDS");
        }
    }
}

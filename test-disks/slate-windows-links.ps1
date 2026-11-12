# How the Slate Windows captures treat a reparse point (#1357). Dot-sourced
# by the read, allocation and ACL captures, so all three classify an entry
# the same way.
#
# A directory listing reports what the parent's $I30 entry says. The file
# itself is asked without following it: GetFileAttributesW returns a
# symbolic link's own attributes, FSCTL_GET_REPARSE_POINT reads its tag
# through a handle opened with FILE_FLAG_OPEN_REPARSE_POINT, and so does
# GetSecurityInfo for its descriptor. A listing that disagrees with the file
# fails by path: that was how a link this crate wrote was listed as a plain
# file, and then followed.
if (-not ('SlateWindowsLinks' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class SlateWindowsLinks {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern uint GetFileAttributesW(string name);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle handle, uint code, IntPtr input,
        int inputLength, byte[] output, int outputLength, out int returned, IntPtr overlapped);
    [DllImport("advapi32.dll")]
    static extern uint GetSecurityInfo(SafeFileHandle handle, int type, uint information,
        IntPtr owner, IntPtr group, IntPtr dacl, IntPtr sacl, out IntPtr descriptor);
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool ConvertSecurityDescriptorToStringSecurityDescriptorW(IntPtr descriptor,
        uint revision, uint information, out IntPtr sddl, IntPtr length);
    [DllImport("kernel32.dll")]
    static extern IntPtr LocalFree(IntPtr memory);

    public const uint ReparsePoint = 0x400;
    public const uint SymlinkTag = 0xA000000C;
    const uint Invalid = 0xFFFFFFFF, ReadControl = 0x20000, ReadAttributes = 0x80;
    const uint ShareAll = 7, OpenExisting = 3;
    const uint BackupSemantics = 0x02000000, OpenReparsePoint = 0x00200000;
    const uint GetReparsePoint = 0x000900A8;
    const int FileObject = 1;
    const uint OwnerGroupDacl = 1 | 2 | 4;

    public static uint Attributes(string path) {
        uint attributes = GetFileAttributesW(path);
        if (attributes == Invalid)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "cannot read the attributes of " + path);
        return attributes;
    }

    static SafeFileHandle Open(string path, uint access) {
        SafeFileHandle handle = CreateFileW(path, access, ShareAll, IntPtr.Zero, OpenExisting,
            BackupSemantics | OpenReparsePoint, IntPtr.Zero);
        if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), "cannot open " + path);
        return handle;
    }

    public static uint Tag(string path) {
        using (SafeFileHandle handle = Open(path, ReadAttributes)) {
            byte[] output = new byte[16 * 1024];
            int returned;
            if (!DeviceIoControl(handle, GetReparsePoint, IntPtr.Zero, 0, output, output.Length,
                    out returned, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "FSCTL_GET_REPARSE_POINT on " + path);
            return BitConverter.ToUInt32(output, 0);
        }
    }

    // The owner, group and DACL of the link itself, as SDDL.
    public static string Sddl(string path) {
        using (SafeFileHandle handle = Open(path, ReadControl)) {
            IntPtr descriptor;
            uint error = GetSecurityInfo(handle, FileObject, OwnerGroupDacl, IntPtr.Zero, IntPtr.Zero,
                IntPtr.Zero, IntPtr.Zero, out descriptor);
            if (error != 0) throw new Win32Exception((int)error, "cannot read the security descriptor of " + path);
            try {
                IntPtr sddl;
                if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor, 1, OwnerGroupDacl,
                        out sddl, IntPtr.Zero))
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "cannot format the descriptor of " + path);
                try { return Marshal.PtrToStringUni(sddl); } finally { LocalFree(sddl); }
            } finally { LocalFree(descriptor); }
        }
    }
}
'@
}

# 'file', 'directory' or 'symlink' for a listed item. Any other reparse
# point, and a listing that disagrees with the file, fail by path.
function Get-SlateWindowsEntryKind($Item, [string]$Relative, [string]$Manifest) {
    $listed = ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    $own = ([SlateWindowsLinks]::Attributes($Item.FullName) -band [SlateWindowsLinks]::ReparsePoint) -ne 0
    if ($listed -ne $own) {
        throw "the directory entry of $Relative says reparse point $listed, but the file says $own"
    }
    if (-not $own) { if ($Item.PSIsContainer) { return 'directory' } else { return 'file' } }
    $tag = [SlateWindowsLinks]::Tag($Item.FullName)
    if ($tag -ne [SlateWindowsLinks]::SymlinkTag) {
        throw ("Windows {0} manifest does not follow reparse point {1} (tag 0x{2:X8})" -f $Manifest, $Relative, $tag)
    }
    'symlink'
}

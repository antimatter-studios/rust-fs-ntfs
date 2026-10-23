# scripts/fs-windows-test-harness/win-verify-reparse-objid.ps1 -- have
# Windows read back the symbolic link and the object id this crate gave one
# file, and find neither on a file this crate gave both and then took them
# from (#810).
#
# Both files carry an $ATTRIBUTE_LIST, so the $REPARSE_POINT and
# $OBJECT_ID went through it (#808, #809). Windows finds an attribute the
# list misplaces nowhere, and one the list still names after its removal
# where it should not be, so a wrong list fails here even when chkdsk has
# nothing to say. Every handle is opened with FILE_FLAG_OPEN_REPARSE_POINT,
# so the queries ask the file and not the file its link names; only the
# read of -Path's contents follows the link, which is what proves Windows
# resolves it.
#
# Args:
#   -ImagePath       Path on the VM to the .img file.
#   -Path            The linked file inside the volume, e.g. "/listed.bin".
#   -Target          The relative target mac-reparse gave it, e.g. "target.txt".
#   -TargetContent   What reading -Path through the link must return.
#   -ObjectId        The object id mac-objid gave it, as 32 hexadecimal digits.
#   -Cleared         The file whose link and object id were removed.
#   -ClearedContent  That file's own contents.
#   -KeepImage       `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag            Directory for verify-reparse-objid-result.txt / -error.txt.
#
# Exit code:
#   0 when Windows reads back exactly what was written and removed
#   1 otherwise (logged to verify-reparse-objid-error.txt)
#   2 for arg errors

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Path,
    [Parameter(Mandatory=$true)] [string]$Target,
    [Parameter(Mandatory=$true)] [string]$TargetContent,
    [Parameter(Mandatory=$true)] [string]$ObjectId,
    [Parameter(Mandatory=$true)] [string]$Cleared,
    [Parameter(Mandatory=$true)] [string]$ClearedContent,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

# FSCTL_GET_REPARSE_POINT and FSCTL_GET_OBJECT_ID on a handle opened with
# FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS. Each returns
# the output buffer, or throws a Win32Exception carrying the error code.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class ReparseObjid {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string file, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle device, uint code, IntPtr input,
        uint inputSize, byte[] output, uint outputSize, out uint returned, IntPtr overlapped);
    public const uint FSCTL_GET_REPARSE_POINT = 0x000900A8;
    public const uint FSCTL_GET_OBJECT_ID = 0x0009009C;
    public static byte[] Query(string file, uint code) {
        // FILE_READ_ATTRIBUTES; share read, write and delete; OPEN_EXISTING;
        // FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS.
        using (var handle = CreateFileW(file, 0x80, 7, IntPtr.Zero, 3, 0x00200000 | 0x02000000, IntPtr.Zero)) {
            if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            var buffer = new byte[16 * 1024];
            uint returned;
            if (!DeviceIoControl(handle, code, IntPtr.Zero, 0, buffer, (uint)buffer.Length, out returned, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            var output = new byte[returned];
            Array.Copy(buffer, output, returned);
            return output;
        }
    }
}
'@

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}
if ($ObjectId -notmatch '^[0-9a-fA-F]{32}$') {
    [Console]::Error.WriteLine("invalid -ObjectId: '$ObjectId' (expected 32 hexadecimal digits)")
    exit 2
}
$relPath = $Path -replace '^[/\\]+', ''
$relCleared = $Cleared -replace '^[/\\]+', ''
if ($relPath -eq '' -or $relCleared -eq '') {
    [Console]::Error.WriteLine("win-verify-reparse-objid: -Path and -Cleared must name files under the volume root; got '$Path', '$Cleared'")
    exit 2
}

function Hex([byte[]]$bytes) {
    return ([BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant()
}

# The UTF-16 name at $offset, $length bytes long, in $buffer.
function Utf16([byte[]]$buffer, [int]$offset, [int]$length) {
    return [System.Text.Encoding]::Unicode.GetString($buffer, $offset, $length)
}

# The Win32 error a query fails with, or $null when it succeeds.
function Query-Error([string]$file, [uint32]$code) {
    try {
        [void][ReparseObjid]::Query($file, $code)
        return $null
    } catch {
        $e = $_.Exception
        while ($null -ne $e.InnerException) { $e = $e.InnerException }
        if ($e -is [System.ComponentModel.Win32Exception]) { return $e.NativeErrorCode }
        throw
    }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $linked = "${letter}:\$relPath"
    $clearedPath = "${letter}:\$relCleared"
    $want = $Target -replace '/', '\'
    try {
        $attrs = [System.IO.File]::GetAttributes($linked)
        if (($attrs -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
            throw "Windows does not mark $linked as a reparse point (attributes $attrs)"
        }
        # REPARSE_DATA_BUFFER: tag, data length, reserved, then the
        # SymbolicLinkReparseBuffer's substitute and print name offsets and
        # lengths, its flags, and the path buffer at byte 20 (MS-FSCC 2.1.2.4).
        $rp = [ReparseObjid]::Query($linked, [ReparseObjid]::FSCTL_GET_REPARSE_POINT)
        if ($rp.Length -lt 20) { throw "the reparse point of $linked is $($rp.Length) bytes" }
        # Compared as text: Windows PowerShell reads 0xA000000C as a
        # negative Int32, which no UInt32 tag equals.
        $tag = '{0:x8}' -f [BitConverter]::ToUInt32($rp, 0)
        if ($tag -ne 'a000000c') { throw "the reparse tag of $linked is 0x$tag, want IO_REPARSE_TAG_SYMLINK (0xa000000c)" }
        $subOff = [BitConverter]::ToUInt16($rp, 8)
        $subLen = [BitConverter]::ToUInt16($rp, 10)
        $printOff = [BitConverter]::ToUInt16($rp, 12)
        $printLen = [BitConverter]::ToUInt16($rp, 14)
        $flags = [BitConverter]::ToUInt32($rp, 16)
        if (20 + $subOff + $subLen -gt $rp.Length -or 20 + $printOff + $printLen -gt $rp.Length) {
            throw "the names of $linked's link run past its $($rp.Length)-byte reparse point"
        }
        $substitute = Utf16 $rp (20 + $subOff) $subLen
        $print = Utf16 $rp (20 + $printOff) $printLen
        if ($flags -ne 1) { throw "the link of $linked has flags $flags, want 1 (SYMLINK_FLAG_RELATIVE)" }
        if ($substitute -cne $want -or $print -cne $want) {
            throw "Windows reads the link of $linked as substitute '$substitute', print '$print'; want '$want'"
        }
        $through = [System.IO.File]::ReadAllText($linked)
        if ($through -cne $TargetContent) {
            throw "reading $linked through its link returns '$through', want '$TargetContent'"
        }

        # FILE_OBJECTID_BUFFER: the 16-byte object id comes first.
        $oid = [ReparseObjid]::Query($linked, [ReparseObjid]::FSCTL_GET_OBJECT_ID)
        if ($oid.Length -lt 16) { throw "the object id buffer of $linked is $($oid.Length) bytes" }
        $got = Hex $oid[0..15]
        if ($got -ne $ObjectId.ToLowerInvariant()) {
            throw "Windows reads the object id of $linked as $got, want $($ObjectId.ToLowerInvariant())"
        }

        $attrs = [System.IO.File]::GetAttributes($clearedPath)
        if (($attrs -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Windows still marks $clearedPath as a reparse point (attributes $attrs)"
        }
        # 4390 is ERROR_NOT_A_REPARSE_POINT.
        $rpError = Query-Error $clearedPath ([ReparseObjid]::FSCTL_GET_REPARSE_POINT)
        if ($rpError -ne 4390) {
            throw "FSCTL_GET_REPARSE_POINT on $clearedPath gave $(if ($null -eq $rpError) { 'a reparse point' } else { "error $rpError" }), want error 4390"
        }
        $oidError = Query-Error $clearedPath ([ReparseObjid]::FSCTL_GET_OBJECT_ID)
        if ($null -eq $oidError) {
            throw "Windows still reads an object id on $clearedPath"
        }
        $own = [System.IO.File]::ReadAllText($clearedPath)
        if ($own -cne $ClearedContent) {
            throw "$clearedPath reads back '$own', want '$ClearedContent'"
        }

        @(
            "Windows reads $linked as a relative link to '$substitute' that it follows, with object id $got;",
            "$clearedPath has no reparse point (error $rpError) and no object id (error $oidError)"
        ) | Out-File "$Diag\verify-reparse-objid-result.txt" -Encoding UTF8
    } catch {
        $_.Exception.Message | Out-File "$Diag\verify-reparse-objid-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}

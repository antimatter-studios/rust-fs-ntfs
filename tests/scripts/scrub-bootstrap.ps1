# Fixed-VHD framing, BPBs and signatures must survive bootstrap removal.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../test-disks/_scrub-ntfs-bootstrap.ps1"
$path = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
try {
    $offset = 65536
    $bytes = [byte[]]::new($offset + 17 * 512 + 512)
    [Array]::Fill[byte]($bytes, 0x71)
    [Text.Encoding]::ASCII.GetBytes('NTFS    ').CopyTo($bytes, $offset + 3)
    [BitConverter]::GetBytes([uint16]512).CopyTo($bytes, $offset + 11)
    [BitConverter]::GetBytes([uint64]16).CopyTo($bytes, $offset + 40)
    $bytes[$offset + 510] = 0x55
    $bytes[$offset + 511] = 0xaa
    [Array]::Copy($bytes, $offset, $bytes, $offset + 8192, 512)
    $expected = [byte[]]$bytes.Clone()
    [Array]::Clear($expected, $offset + 84, 426)
    [Array]::Clear($expected, $offset + 512, 7680)
    [Array]::Clear($expected, $offset + 8192 + 84, 426)
    [IO.File]::WriteAllBytes($path, $bytes)
    Clear-NtfsBootstrap $path $offset (17 * 512)
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) -ne [Convert]::ToBase64String($expected)) {
        throw 'scrub changed bytes outside bootstrap regions, or retained bootstrap code'
    }
    Clear-NtfsBootstrap $path $offset (17 * 512)
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) -ne [Convert]::ToBase64String($expected)) {
        throw 'scrub is not idempotent'
    }
    foreach ($badOffset in @(($offset + 3), ($offset + 510), ($offset + 8192 + 510))) {
        $bad = [byte[]]$bytes.Clone()
        $bad[$badOffset] = 0
        [IO.File]::WriteAllBytes($path, $bad)
        $refused = $false
        try { Clear-NtfsBootstrap $path $offset (17 * 512) } catch { $refused = $true }
        if (-not $refused) { throw "invalid image accepted at $badOffset" }
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) -ne [Convert]::ToBase64String($bad)) {
            throw 'invalid image was modified before refusal'
        }
    }
    Write-Host 'scrub-bootstrap: all checks passed'
} finally {
    [IO.File]::Delete($path)
}

# scripts/v2/_lib.ps1 -- shared helpers for v2 win-side op scripts.
#
# Dot-source this from win-chkdsk.ps1, win-enumerate.ps1 and any future
# win-* helper. Functions are deliberately small and stateless: each
# returns the bits the caller needs, and the caller controls the
# try/finally lifecycle.
#
# Two execution shapes share these helpers:
#   - "first op for a scenario": .img is on the VM (shipped), no .vhd
#     yet. Init-VhdFromImg creates the wrapper + streams the .img bytes
#     into the partition, leaves the volume ready to mount.
#   - "follow-on op": .vhd already exists on disk from a prior op (the
#     prior op was invoked with KeepImage=true). Init-VhdFromImg sees
#     the existing .vhd and skips the create+stream phase.
#
# Mount-VhdAndGetLetter does the dismount/remount + letter detection
# dance that ntfs.sys requires after a fresh raw write — used by
# every op after the volume's bytes are in place.

# rust-img-vhd is on the PATH via setup-windows-vm.ps1 (cargo install
# --bin rust-img-vhd puts it in ~/.cargo/bin which rustup adds to PATH).
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

# ── Drive-letter mutex ────────────────────────────────────────────────────────
# Serialises the "snapshot lettersBefore → Mount-DiskImage → detect new letter"
# sequence in Mount-VhdAndGetLetter. Without this, two concurrent op scripts
# can both capture the same lettersBefore set and then both claim the same
# newly-assigned letter as "theirs", causing Format-Volume / chkdsk / etc. to
# target the wrong drive letter or fail with "No MSFT_Volume objects found".
#
# FileMode.CreateNew is atomic on Windows NTFS — exactly one caller wins the
# race to create the file; all others get IOException and retry.

$script:DriveLockPath = "$env:TEMP\vhd-drive-lock"

function Acquire-DriveLock {
    $deadline = [DateTime]::UtcNow.AddSeconds(180)
    while ($true) {
        # Reclaim stale lock left by a crashed process (older than 180 s).
        # Wrap Get-Item in try/catch: with FileShare.None another thread
        # holds the file open exclusively during its CreateNew→Close window
        # and Get-Item raises UnauthorizedAccessException under Stop pref.
        # If we can't read the age the lock is clearly not stale — skip.
        if (Test-Path $script:DriveLockPath) {
            try {
                $age = ([DateTime]::UtcNow - (Get-Item $script:DriveLockPath).LastWriteTimeUtc).TotalSeconds
                if ($age -gt 180) {
                    Remove-Item -LiteralPath $script:DriveLockPath -Force -EA SilentlyContinue
                }
            } catch {
                # Lock held exclusively — not stale; fall through to retry.
            }
        }
        try {
            $fs = [System.IO.File]::Open(
                $script:DriveLockPath,
                [System.IO.FileMode]::CreateNew,
                [System.IO.FileAccess]::Write,
                [System.IO.FileShare]::None)
            $fs.Close()
            return
        } catch [System.IO.IOException] {
            if ([DateTime]::UtcNow -ge $deadline) {
                throw "Acquire-DriveLock: timed out after 180s waiting for $($script:DriveLockPath)"
            }
            Start-Sleep -Milliseconds 250
        }
    }
}

function Release-DriveLock {
    Remove-Item -LiteralPath $script:DriveLockPath -Force -EA SilentlyContinue
}

function Get-VhdPathFor {
    param([Parameter(Mandatory=$true)] [string]$ImagePath)
    return [System.IO.Path]::ChangeExtension($ImagePath, ".vhd")
}

# Wrap an .img into a fixed VHD, mount, GPT-init, partition, and stream
# the .img bytes into the partition's offset. Returns @{ Vhd; Disk; }
# (the caller passes Vhd through to Mount-VhdAndGetLetter and Disk
# isn't used after this — kept for diag/debugging).
#
# Idempotent: if the .vhd already exists on disk (because a prior op
# in the same scenario ran with KeepImage=true), the create+stream
# phase is skipped and we just return the existing Vhd path. The
# caller still needs to call Mount-VhdAndGetLetter to get a letter.
function Initialize-VhdFromImg {
    param(
        [Parameter(Mandatory=$true)] [string]$ImagePath,
        [Parameter(Mandatory=$true)] [string]$Diag
    )

    if (-not (Test-Path $ImagePath)) {
        throw "image not found on VM: $ImagePath"
    }

    $Vhd = Get-VhdPathFor -ImagePath $ImagePath

    # Belt-and-braces: tear down any orphaned mount of this Vhd path
    # before we look at the file. A crashed prior run could leave the
    # VHD both on disk *and* attached — the reuse fast path below
    # would then return early and the caller's Mount-VhdAndGetLetter
    # would fail trying to remount an already-attached image. Run this
    # before the existence check so both paths self-heal.
    try {
        Get-DiskImage -ImagePath $Vhd -EA SilentlyContinue |
            Where-Object Attached |
            Dismount-DiskImage -EA SilentlyContinue | Out-Null
    } catch { }

    # Already-streamed VHD from a prior op — fast path: skip the
    # rust-img-vhd + stream phase, just return the path.
    #
    # Stale-VHD detection: if the .img is *newer* than the .vhd, a
    # mac-side op (or a re-ship-to-vm) modified the source bytes after
    # the VHD was last streamed. Reusing the VHD would mount the
    # OLD bytes — the round-trip win-format scenarios depend on this
    # detection (`win:format -> ship-to-host -> mac:write -> ship-to-vm
    # -> win:chkdsk`: the second win-* op must see the post-mac-write
    # bytes, not the pre-mac-write ones from the prior win-format).
    # Wipe the stale VHD and fall through to rebuild.
    if (Test-Path $Vhd) {
        $imgWriteTime = (Get-Item $ImagePath).LastWriteTimeUtc
        $vhdWriteTime = (Get-Item $Vhd).LastWriteTimeUtc
        if ($imgWriteTime -le $vhdWriteTime) {
            return @{ Vhd = $Vhd }
        }
        # .img is newer — VHD is stale. Delete and rebuild.
        Remove-Item -LiteralPath $Vhd -Force -EA SilentlyContinue
        if (Test-Path -LiteralPath $Vhd) {
            throw "Stale VHD could not be removed: $Vhd (img mtime $imgWriteTime > vhd mtime $vhdWriteTime)"
        }
    }

    # Sized just larger than the .img so the GPT slack fits, in bytes.
    # `create` rounds a size up to a whole CHS geometry, so the wrapper
    # can come out a little larger than asked; the slack absorbs it.
    $rawSize          = (Get-Item $ImagePath).Length
    $rawSizeMb        = [int][Math]::Ceiling($rawSize / 1MB)
    $wrapperMb        = $rawSizeMb + 64
    $wrapperSizeBytes = [int64]$wrapperMb * 1MB

    & rust-img-vhd img $Vhd create $wrapperSizeBytes --type fixed *> "$Diag\wrapper-create.txt"
    if ($LASTEXITCODE -ne 0) {
        throw "rust-img-vhd img create failed exit=$LASTEXITCODE (see wrapper-create.txt)"
    }
    fsutil sparse setflag $Vhd 0 | Out-Null

    # PowerShell variable names are case-insensitive — assigning the
    # Mount-DiskImage CimInstance to `$vhd` would silently overwrite
    # the `$Vhd` path string, breaking the `Dismount-DiskImage -ImagePath
    # $Vhd` call below and the returned hashtable's `Vhd` field. Use
    # a distinct name for the CimInstance.
    $mountedImg = Mount-DiskImage -ImagePath $Vhd -PassThru
    Start-Sleep -Seconds 2
    Initialize-Disk -Number $mountedImg.Number -PartitionStyle GPT
    Start-Sleep -Seconds 2
    $disk = Get-Disk -Number $mountedImg.Number
    $part = New-Partition -DiskNumber $mountedImg.Number -UseMaximumSize -AssignDriveLetter:$false
    if ($part.Size -lt $rawSize) {
        throw "partition smaller than raw image ($($part.Size) < $rawSize)"
    }

    # Take the disk offline before opening the raw `\\.\PhysicalDriveN`
    # handle — the volume layer otherwise auto-detects the new partition
    # and locks it, which makes chunked writes return "Access to the
    # path is denied." See PR #14 for the full story.
    Set-Disk -Number $disk.Number -IsOffline $true
    $writeFailed = $false
    try {
        $rawPath = "\\.\PhysicalDrive$($disk.Number)"
        $src = [System.IO.File]::Open($ImagePath, [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
        try {
            try {
                $dst = [System.IO.File]::Open($rawPath, [System.IO.FileMode]::Open,
                    [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::ReadWrite)
                try {
                    $dst.Seek($part.Offset, [System.IO.SeekOrigin]::Begin) | Out-Null
                    # Pad the trailing chunk's `[n, aligned)` window with
                    # zeros so each Write is a multiple of the physical
                    # sector size; Read rewrote `[0, n)` already.
                    $sectorSize = $disk.PhysicalSectorSize
                    $bufSize = 16MB
                    $buf = New-Object byte[] $bufSize
                    while ($true) {
                        $n = $src.Read($buf, 0, $bufSize)
                        if ($n -le 0) { break }
                        $aligned = [int][Math]::Ceiling($n / $sectorSize) * $sectorSize
                        if ($aligned -gt $n) {
                            [Array]::Clear($buf, $n, $aligned - $n)
                        }
                        $dst.Write($buf, 0, $aligned)
                    }
                    $dst.Flush($true)
                } finally { $dst.Close() }
            } catch {
                $writeFailed = $true
                throw
            }
        } finally { $src.Close() }
    } finally {
        if ($writeFailed) {
            Set-Disk -Number $disk.Number -IsOffline $false -EA SilentlyContinue
        } else {
            Set-Disk -Number $disk.Number -IsOffline $false -EA Stop
        }
    }

    # Dismount the just-streamed VHD. The caller's Mount-VhdAndGetLetter
    # remounts it so ntfs.sys re-recognises the populated partition and
    # assigns a drive letter.
    Dismount-DiskImage -ImagePath $Vhd | Out-Null

    return @{ Vhd = $Vhd; Disk = $disk }
}

# Mount the VHD, wait for ntfs.sys to assign a drive letter (with a
# manual Set-Partition fallback if auto-assignment doesn't happen
# within 10s). Returns the bare letter (e.g. "E"). Throws if no letter
# can be obtained.
function Mount-VhdAndGetLetter {
    param([Parameter(Mandatory=$true)] [string]$Vhd)

    # Settle pause outside the lock: let any prior Dismount-DiskImage complete
    # before we enter the critical section. The old monolithic win-chkdsk.ps1
    # had this between dismount and remount; the lib refactor split those
    # across functions and dropped it. Kept here so ntfs.sys has a chance to
    # finish tearing down before we snapshot the letter table.
    Start-Sleep -Seconds 1

    # Mount the VHD. -PassThru gives us a disk number that is unique to
    # THIS VHD — no shared state with concurrent scripts.
    # Distinct variable name avoids case-insensitive collision with $Vhd.
    $mountedImg = Mount-DiskImage -ImagePath $Vhd -PassThru
    $diskNumber = $mountedImg.Number

    # Phase 1 (lock-free): poll for the drive letter Windows auto-assigns
    # to NTFS volumes. We query specifically our disk number so there is no
    # race with other concurrent mounts — each VHD has its own disk number
    # and its own partitions. No lettersBefore/lettersAfter diff needed.
    $letter = $null
    for ($i = 0; $i -lt 10; $i++) {
        Start-Sleep -Seconds 1
        $part = Get-Partition -DiskNumber $diskNumber -EA SilentlyContinue |
            Where-Object { $_.Type -ne 'Reserved' -and $_.DriveLetter } |
            Select-Object -First 1
        if ($part) { $letter = "$($part.DriveLetter)"; break }
    }
    if ($letter) { return $letter }

    # Phase 2 (locked): Set-Partition fallback for RAW volumes (win-format
    # scenarios where the partition has no filesystem yet and Windows will
    # not auto-assign a letter). Lock prevents two concurrent RAW-volume
    # scripts from picking the same unused letter simultaneously.
    Acquire-DriveLock
    try {
        $disk2 = Get-Disk -Number $diskNumber
        $partition = Get-Partition -DiskNumber $disk2.Number |
            Where-Object { $_.Type -ne 'Reserved' } | Select-Object -First 1
        $used = (Get-Volume | ForEach-Object { $_.DriveLetter }) +
                (Get-PSDrive -PSProvider FileSystem | ForEach-Object { $_.Name })
        foreach ($c in [char[]](68..90)) {
            if ($c -notin $used) {
                try {
                    Set-Partition -DiskNumber $disk2.Number `
                        -PartitionNumber $partition.PartitionNumber `
                        -NewDriveLetter $c -ErrorAction Stop
                    $letter = "$c"; break
                } catch { }
            }
        }
    } finally {
        Release-DriveLock
    }
    if (-not $letter) { throw "no drive letter assigned" }
    return "$letter"
}

# Read the partition contents out of the VHD and write them back over
# the .img file. The inverse of the byte
# stream `Initialize-VhdFromImg` does on the way in — without this
# the .img on the VM stays at whatever bytes it had at ship-to-vm
# time (zeros for `win-format-*` scenarios), even though the VHD
# wrapper now holds the win-* op's writes. The result: `ship-to-host`
# would pull back a stale .img that doesn't reflect anything Windows
# did, and the follow-on mac-* ops (mac-enumerate, mac-rm, …) all
# fail on the zero-filled buffer.
#
# Windows detaches the disk first: the detach dismounts the volume the
# way Windows ends its own use of a disk, flushing it and writing its
# final checkpoint. The fixed VHD then holds the disk's bytes from offset
# 0, so the partition is read from the wrapper file itself, and
# `(Get-Item .img).Length` bytes go back into the .img. No forced
# dismount and no offline/online cycle: a forced `fsutil volume
# dismount` before taking the disk offline left the next detach hung in
# the storage service (full runs 38874733868, 38897625186, 38908178473),
# and taking it offline without one raced NTFS's final checkpoint
# (sparse 4k scenario, CI run 38819803270).
# Returns with the VHD detached.
#
# Idempotent for read-only ops (chkdsk readonly etc.) — the partition
# bytes match what was streamed in, so the .img stays byte-identical.
# We accept the wasted IO for the read-only case to keep the
# Dismount-VhdAndCleanup contract uniform: "after this returns, the
# .img file reflects the post-op state of the volume."
function Sync-VhdToImg {
    param(
        [Parameter(Mandatory=$true)] [string]$Vhd,
        [Parameter(Mandatory=$true)] [string]$ImagePath
    )

    # The VHD must still be attached, for its partition's offset and
    # size. Callers run this before `Dismount-VhdAndCleanup`, which
    # finds it already detached.
    $mountedImg = Get-DiskImage -ImagePath $Vhd -EA SilentlyContinue
    if (-not $mountedImg -or -not $mountedImg.Attached) {
        throw "Sync-VhdToImg: VHD not attached: $Vhd"
    }
    $disk = Get-Disk -Number $mountedImg.Number
    $part = Get-Partition -DiskNumber $disk.Number |
        Where-Object { $_.Type -ne 'Reserved' } | Select-Object -First 1
    if (-not $part) {
        throw "Sync-VhdToImg: no data partition on disk $($disk.Number)"
    }
    $imgSize = (Get-Item $ImagePath).Length
    if ($part.Size -lt $imgSize) {
        throw ("Sync-VhdToImg: partition smaller than .img " +
               "(part=$($part.Size) img=$imgSize)")
    }

    # Windows' own detach, then the bytes it left in the fixed VHD. A
    # detach that fails is the sync's failure: the wrapper may not hold
    # the volume's final state.
    $offset = [long]$part.Offset
    Dismount-DiskImage -ImagePath $Vhd -ErrorAction Stop | Out-Null
    $src = [System.IO.File]::Open($Vhd, [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    try {
        $dst = [System.IO.File]::Open($ImagePath, [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
        try {
            $src.Seek($offset, [System.IO.SeekOrigin]::Begin) | Out-Null
            $buf = New-Object byte[] 16MB
            $remaining = $imgSize
            while ($remaining -gt 0) {
                $n = $src.Read($buf, 0, [int][Math]::Min($buf.Length, $remaining))
                if ($n -le 0) {
                    # A short wrapper is a truncation, not a normal EOF:
                    # never ship a partially synced .img.
                    throw "Sync-VhdToImg: short read from $Vhd; $remaining bytes not copied"
                }
                $dst.Write($buf, 0, $n)
                $remaining -= $n
            }
            $dst.Flush($true)
        } finally { $dst.Close() }
    } finally { $src.Close() }
}

# Tear down a VHD wrapper (best-effort dismount + remove the file).
# `KeepImage=$true` leaves the source .img and the .vhd in place so a
# follow-on op in the same scenario can mount them again. The final op
# in the scenario should use KeepImage=$false (or call Remove-ScenarioImage
# explicitly) to avoid leaving GiB-sized artefacts on the VM.
#
# **`SyncBack`** (default `$true`): detach, then copy the partition
# bytes out of the fixed VHD back to the `.img` file
# so the `.img` reflects the post-op state of the volume. This is the
# fix for `win-format-*` scenarios where the win-* op's writes land
# in the VHD wrapper and never propagate to the `.img` — see
# `Sync-VhdToImg` for the full rationale. Callers that are sure the
# op was strictly read-only (no volume metadata updates, no
# last-access timestamps) can pass `$false` to skip the sync IO, but
# the default is safe-and-redundant rather than fast-and-wrong.
function Dismount-VhdAndCleanup {
    param(
        [Parameter(Mandatory=$true)] [string]$Vhd,
        [Parameter(Mandatory=$true)] [string]$ImagePath,
        [bool]$KeepImage = $false,
        [bool]$SyncBack = $true
    )

    if ($SyncBack) {
        try {
            Sync-VhdToImg -Vhd $Vhd -ImagePath $ImagePath
        } catch {
            Write-Warning "Sync-VhdToImg failed (continuing with dismount): $_"
        }
    }

    try {
        Get-DiskImage -ImagePath $Vhd -EA SilentlyContinue |
            Where-Object Attached |
            Dismount-DiskImage -EA SilentlyContinue | Out-Null
    } catch { }

    if ($KeepImage) { return }

    # Best-effort delete with explicit verification on both files.
    # Silently swallowing a cleanup failure was the bug PR #9 fixed for
    # the .img; the .vhd needs the same treatment because
    # Initialize-VhdFromImg uses .vhd existence as its idempotency
    # signal — a silently-failed wrapper cleanup would cause the next
    # scenario to skip the create+stream phase and remount stale data.
    Remove-Item -LiteralPath $Vhd -Force -EA SilentlyContinue
    if (Test-Path -LiteralPath $Vhd) {
        Write-Warning "Failed to remove VHD wrapper: $Vhd"
    }
    Remove-Item -LiteralPath $ImagePath -Force -EA SilentlyContinue
    if (Test-Path -LiteralPath $ImagePath) {
        Write-Warning "Failed to remove shipped image: $ImagePath"
    }
}

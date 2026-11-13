using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;

public static class SlateAclRestrictedToken {
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool CreateRestrictedToken(SafeAccessTokenHandle existing, uint flags,
        uint disabledCount, IntPtr disabled, uint privilegeCount, IntPtr privileges,
        uint restrictedCount, IntPtr restricted, out SafeAccessTokenHandle token);

    public static void Run(Action action) {
        using (var identity = WindowsIdentity.GetCurrent(TokenAccessLevels.Duplicate | TokenAccessLevels.Query)) {
            SafeAccessTokenHandle token;
            if (!CreateRestrictedToken(identity.AccessToken, 1, 0, IntPtr.Zero,
                0, IntPtr.Zero, 0, IntPtr.Zero, out token))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            using (token) { WindowsIdentity.RunImpersonated(token, action); }
        }
    }
}

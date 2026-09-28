package dev.detour.core

/**
 * Temporary product switches.
 *
 * [TUN_ONLY] is a deliberate, temporary product decision: only the TUN (VPN)
 * mode is shown for now, and the proxy mode is hidden behind this flag rather
 * than deleted. The proxy code paths are all still here — the mode picker on the
 * home screen, the proxy-address card, the proxy-port setting row, and the
 * stored-mode restore in [DetourApp] — so restoring the picker is a one-line
 * change: flip this back to `false`.
 *
 * While it is `true`, the running mode is forced to [KernelState.Mode.VPN]
 * regardless of what is stored in prefs. That forcing is not cosmetic: with the
 * picker hidden there is no control that can change the mode back, so a device
 * that previously stored `proxy` would otherwise run the proxy forever with no
 * way out. The stored value is left untouched so flipping the flag restores the
 * user's own choice.
 */
object BuildFlags {
    const val TUN_ONLY = true
}

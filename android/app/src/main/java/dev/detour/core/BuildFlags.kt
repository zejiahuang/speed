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
 * regardless of what is stored in prefs. That forcing is what keeps a device
 * that previously stored `proxy` from starting up in a mode whose picker it
 * cannot see. The stored value is left untouched so flipping the flag restores
 * the user's own choice.
 *
 * ## What this flag does **not** hide
 *
 * The command surface stays open. `ControlConsole`'s `mode` command is not gated
 * on this flag, so `mode proxy` typed into the in-app console still switches the
 * running engine to the proxy path — in release builds too, because that console
 * is gated on 开发者视图 rather than on `BuildConfig.DEBUG` (its class comment
 * explains why the console and the adb receiver are gated differently). That is
 * deliberate, not an oversight: gating one command would make the console
 * disagree with the receiver about what it accepts, and keeping the two in
 * agreement is the reason `ControlConsole` exists as a shared object at all.
 *
 * So this flag hides the *discoverable* entry points, not the mode. Do not read
 * it as "proxy is unreachable". An earlier revision of this comment claimed
 * there was "no control that can change the mode back" — that was wrong; the
 * console can, and the consequence is that a console-set `proxy` stays in prefs
 * and is overridden to VPN on the next cold start rather than cleared.
 */
object BuildFlags {
    const val TUN_ONLY = true
}

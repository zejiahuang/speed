package dev.detour.core

/**
 * The switch that decides whether the proxy mode is offered.
 *
 * It is `false`: proxy mode is a shipped, user-selectable mode. It spent a while
 * as `true`, back when the proxy answered `403 Forbidden` to every domain that
 * was not in the rule set — a mode that refuses a plain `www.baidu.com` is not
 * one to put a picker in front of. That refusal is gone. An unlisted domain now
 * goes out directly, which is what the tunnel has always done with one (the
 * kernel's `Router::plan` returns `Plan::direct`), and the proxy also forwards
 * plain HTTP instead of only `CONNECT`. So the picker, the proxy-address card
 * and the port row are all back on screen.
 *
 * **Why the flag stays instead of being deleted.** Four call sites read it — the
 * mode restore in [DetourApp], the mode control in the home screen header, the
 * proxy-address card below it, and the proxy-port row in the settings screen —
 * and each is written so that this one constant hides the mode again without any
 * of them changing. Keeping it costs one branch per site; deleting it would mean
 * re-deriving those branches the next time the mode has to be pulled, which is
 * exactly the situation this comment is written from.
 *
 * **What `true` would not do.** It would hide the *discoverable* entry points,
 * not the mode. `ControlConsole`'s `mode` command is not gated on this flag, so
 * `mode proxy` typed into the in-app console switches the running engine to the
 * proxy path in release builds too — that console is gated on 开发者视图 rather
 * than on `BuildConfig.DEBUG`. Gating one command there would make the console
 * disagree with the adb receiver about what it accepts, and keeping those two in
 * agreement is why `ControlConsole` is a shared object at all.
 *
 * A console-set `proxy` also stays in prefs, and under a `true` flag it would be
 * overridden to VPN on the next cold start rather than cleared. The stored value
 * is deliberately left alone so the user's own choice comes back the moment the
 * flag is flipped.
 */
object BuildFlags {
    const val TUN_ONLY = false
}

package dev.detour.core

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.PowerManager
import android.provider.Settings

/**
 * Whether the system is allowed to leave this app alone, and how to ask for it.
 *
 * ## Why the app cares
 *
 * The tunnel is a foreground service, and what ends a tunnel is the system
 * deciding to reclaim it. Doze and the app-standby buckets are allowed to stop a
 * foreground service on a device that has not exempted the app, and the failure is
 * invisible from the inside: the tunnel does not raise an error, it stops — so
 * from the user's side a setup that worked simply began refusing to carry traffic,
 * with nothing in the app to point at. The exemption is the only lever an app has
 * over that, which is why it is worth a reminder.
 *
 * ## What is claimed, and what is not
 *
 * Exactly one thing is tested here: `PowerManager.isIgnoringBatteryOptimizations`,
 * i.e. the app showing as 无限制 / Not optimized in the system's battery settings.
 * That is the only part of "keep it alive" that has an API. The 自启动 and 后台运行
 * switches that several Chinese OEMs add on top have no public interface, so they
 * cannot be read, cannot be set, and are therefore **not** claimed to be checked —
 * the reminder says so in its own words rather than implying it verified them.
 */
object BatteryPolicy {

    /**
     * Whether the app is exempt from battery optimizations.
     *
     * **An unanswerable query counts as exempt**, the same way unreadable
     * disclaimer text counts as accepted (see [Disclaimer]). The read should not
     * fail, but on some ROMs it does, and the two ways of being wrong are not
     * symmetric: reporting "not exempt" on a device that *is* exempt produces a
     * reminder that can never be satisfied — the user sets it, the read keeps
     * failing, the window keeps coming back — while reporting "exempt" on a device
     * that is not merely withholds a reminder. The second mistake is the cheaper
     * one, so a failure returns `true`; the caller still logs the state it read, so
     * the condition is not silent.
     */
    fun isUnrestricted(context: Context): Boolean = runCatching {
        val power = context.getSystemService(Context.POWER_SERVICE) as? PowerManager
            ?: return@runCatching true
        power.isIgnoringBatteryOptimizations(context.packageName)
    }.getOrDefault(true)

    /**
     * The system's battery-optimization list.
     *
     * `ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS` rather than
     * `ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, and the difference is not
     * cosmetic. The request action opens a dialog naming this app, which is one tap
     * shorter — but it only works if the manifest declares
     * `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, and that permission exists to be
     * granted to apps whose *core* function cannot work without the exemption.
     * Declaring it to save a tap would be claiming that status on the app's behalf
     * and asking the system to take our word for it. The list needs no permission
     * and asks nothing of the manifest, so the app makes its case in its own words
     * and the system stays the one that decides.
     */
    fun settingsIntent(): Intent = Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)

    /**
     * Hands the user to the system's battery settings, and reports whether anything
     * opened.
     *
     * **Two attempts, and the second is a crash guard rather than a second
     * design.** The action above has existed since API 23 and is present on
     * essentially every device, but "essentially every" is not "every": a ROM that
     * dropped the battery-optimization screen would make this reminder's only
     * button throw `ActivityNotFoundException`, and a button that does nothing when
     * tapped is exactly the kind of control this project refuses to put on screen.
     * So the failure path falls back to the app's own details page, which several
     * ROMs use as the home of the same switches. The caller logs the outcome when
     * both attempts fail; nothing here is swallowed.
     */
    fun openSettings(context: Context): Boolean {
        if (runCatching { context.startActivity(settingsIntent()) }.isSuccess) return true
        return runCatching { context.startActivity(appDetailsIntent(context)) }.isSuccess
    }

    /** The app's own page in Settings; the fallback for [openSettings]. */
    private fun appDetailsIntent(context: Context): Intent =
        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
            .setData(Uri.fromParts("package", context.packageName, null))
}

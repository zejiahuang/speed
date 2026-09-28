package dev.detour.control

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import dev.detour.BuildConfig
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import org.json.JSONObject
import java.io.File

/**
 * The adb control surface.
 *
 * An exported receiver so the whole app can be driven from a shell, which is what
 * makes it testable without a human tapping the screen. Debug builds only: a
 * release build that any app on the device can tell to connect would be a
 * remote-control surface, not a convenience.
 *
 * ```bash
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd status
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd connect
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd disconnect
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd mode --es value vpn
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd rules --es value refresh
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd rules --es value add:https://example.com/hosts --es key 我的源
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd rules --es value remove:custom:https://example.com/hosts
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd log --es value clear
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd set --es key dark_mode --es value always
 * adb shell am broadcast -a dev.detour.CONTROL --es cmd dump
 * ```
 *
 * **Reading the answer.** A broadcast cannot return one, so every command writes
 * a single-line JSON result to logcat under [TAG] *and* appends it to
 * `files/control.jsonl`. The logcat route is the convenient one:
 *
 * ```bash
 * adb logcat -s DetourControl:V -d | tail -1
 * ```
 *
 * and the file route survives logcat being rotated, and can be pulled with
 * `adb shell run-as dev.detour cat files/control.jsonl`.
 *
 * `dump` additionally writes a full snapshot — status, counters, settings, rule
 * count — so one call answers "what is the app's entire state right now".
 *
 * The commands themselves live in [ControlConsole], shared with the in-app
 * console on the settings screen. This class is only the transport: an intent in,
 * a line of logcat and a line of `control.jsonl` out. Keeping the two surfaces on
 * one implementation is what stops the phone and the laptop from drifting apart
 * the first time a command is added.
 */
class ControlReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION) return

        if (!BuildConfig.DEBUG) {
            // The manifest export is a debug affordance; refusing here means a
            // release build cannot be driven even if the manifest is edited.
            respond(context, JSONObject().put("error", "control is debug-only"))
            return
        }

        val command = intent.getStringExtra("cmd")?.trim()?.lowercase() ?: "status"
        val value = intent.getStringExtra("value")
        val key = intent.getStringExtra("key")
        val app = context.applicationContext

        // On the main thread first, and briefly: the answer has to leave before
        // the broadcast is over or it never leaves at all.
        if (command == "status") {
            respond(app, ControlConsole.statusJson().put("cmd", command))
            return
        }

        // Everything else may fetch or parse, so it goes on the app's own scope
        // rather than into the receiver's. `goAsync` would be the tidier-looking
        // choice and the wrong one: the system gives a pending result about ten
        // seconds, and the rule document is a megabyte over a phone network. The
        // receiver returns immediately and the answer arrives in logcat when the
        // work is done.
        ControlScope.launch {
            val result = runCatching { ControlConsole.run(app, command, value, key) }
                .getOrElse { error ->
                    JSONObject().put("error", error.message ?: error.javaClass.simpleName)
                }
            respond(app, result.put("cmd", command))
        }
    }

    /**
     * One line of JSON, to both places that can be read from a shell.
     *
     * `eprintln` is not enough on its own: the app's stderr is not reliably
     * captured by logcat, and a control surface whose answers vanish is worse
     * than none.
     */
    private fun respond(context: Context, payload: JSONObject) {
        val line = payload.toString()
        Log.println(Log.INFO, TAG, line)
        runCatching {
            File(context.filesDir, LOG_NAME).appendText(line + "\n")
        }
    }

    companion object {
        const val ACTION = "dev.detour.CONTROL"
        const val TAG = "DetourControl"

        /** Read back with `adb shell run-as dev.detour cat files/control.jsonl`. */
        const val LOG_NAME = "control.jsonl"

        /**
         * Where the slow commands run.
         *
         * A receiver has no lifetime worth speaking of, so the work outlives it
         * on a scope of its own. `Dispatchers.IO` because every one of these
         * commands either reads a file or fetches one.
         */
        private val ControlScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    }
}

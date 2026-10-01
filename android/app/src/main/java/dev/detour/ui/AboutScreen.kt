package dev.detour.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.detour.BuildConfig
import dev.detour.R
import dev.detour.core.Kernel
import dev.detour.core.KernelState
import dev.detour.core.Prefs
import dev.detour.core.RuleIndex
import dev.detour.core.UpdateChecker
import dev.detour.core.UpdateState
import dev.detour.ui.components.DetourActionRow
import dev.detour.ui.components.DetourAlertDialog
import dev.detour.ui.components.DetourButton
import dev.detour.ui.components.DetourButtonVariant
import dev.detour.ui.components.DetourDivider
import dev.detour.ui.components.DetourKeyValueRow
import dev.detour.ui.components.DetourPageHeader
import dev.detour.ui.components.DetourSectionCard
import dev.detour.ui.components.DetourToggleRow
import dev.detour.ui.components.ReleaseNotesText
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 关于: a second-level page over the whole app, opened from 设置 and closed back
 * to it.
 *
 * **It is opaque, and that is a consequence of where it sits rather than a
 * style choice.** The app body draws this page as a sibling of the floating bar
 * and *outside* the node marked with `layerBackdrop`, so there is no backdrop
 * within reach here. A `DetourCardStyle.Glass` surface on this page would
 * therefore take `drawGlass`'s fallback branch — the flat tint with a specular
 * border — and read as "the glass is broken" rather than "this page has no
 * glass". So the page paints `colorScheme.surface` across the whole screen and
 * no glass is used anywhere inside it. (The section cards come from
 * [DetourSectionCard], which is the app's one group-card shape and does bake in
 * the glass style; with no backdrop it degrades to the same flat tint, which on
 * an opaque page reads as an ordinary card. That is the honest limit of reusing
 * the shared row vocabulary outside the backdrop scope.)
 *
 * **Why the safe-area insets are taken here and not from the `Scaffold`.** Every
 * other screen is laid out by the app body's `Scaffold`, which hands its content
 * the top inset and deliberately drops the bottom one because the content is
 * meant to scroll under the floating bar. This page is not inside that
 * `Scaffold` — it covers it — so it has to apply its own. It takes the whole
 * safe area rather than just the status bar: the top so the title is not under
 * the clock, and the bottom because there is no floating bar left to clear on
 * this page (it is painted over), so the last row would otherwise sit under the
 * gesture bar.
 *
 * **`BackHandler` rather than a close callback only.** The page looks like a
 * dialog and covers the bar, so the system back gesture has to close it; without
 * this it would fall through to the activity and quit the app from a page that
 * reads as an overlay.
 */
@Composable
fun AboutScreen(onClose: () -> Unit) {
    // Enabled unconditionally: while this page is shown it is the topmost thing
    // on screen, so there is nothing else for back to mean.
    BackHandler(enabled = true) { onClose() }

    val context = LocalContext.current
    val prefs = Prefs.of(context)
    val scope = rememberCoroutineScope()

    val updateUrl = prefs.updateUrl
    var showUrlDialog by remember { mutableStateOf(false) }
    // The check state is no longer held by this page. `checking` and `result` both
    // come from `UpdateState`: this page's manual button and the silent startup check
    // are one entry point and one piece of state, so this reads it rather than
    // keeping its own copy. The reason is on `UpdateState`'s class comment — two
    // copies means the two tests eventually disagree.

    // The rule document's size, read off the cache.
    //
    // `RuleIndex.parse` does real file I/O and JSON parsing — tens of
    // milliseconds on a large document — so it must not run during composition
    // and must not run again on every recomposition. `produceState` is the fix
    // for both: its producer runs once, on the composition's own coroutine, and
    // the I/O is pushed to `Dispatchers.IO` so the frame is not held. A plain
    // `remember { RuleIndex.parse(context) }` would satisfy "once" and still be
    // wrong, because it would parse on the main thread while the first frame is
    // being built.
    //
    // `runCatching` is not defensive padding here. `RulesRepository.load` throws
    // when there is no usable cache — a fresh install that has never fetched a
    // rule document — and an About page must not be the thing that crashes on
    // first launch. A null result renders as a dash.
    val ruleCounts by produceState<Pair<Int, Int>?>(initialValue = null) {
        value = withContext(Dispatchers.IO) {
            runCatching {
                val index = RuleIndex.parse(context)
                index.groups.sumOf { it.domainCount } to index.groups.sumOf { it.addressCount }
            }.getOrNull()
        }
    }

    // The same resolution the rules page uses for its source chips: a built-in
    // carries a string resource, a user-added source carries its own label.
    // `prefs.ruleSource` would be wrong here — that is the source's *id*
    // (`github-hosts`), a storage key rather than something to show a person.
    val source = prefs.selectedSource
    val sourceLabel = source.labelRes?.let { stringResource(it) } ?: source.label

    val ruleScale = ruleCounts?.let { (domains, addresses) ->
        stringResource(R.string.rules_group_counts, domains, addresses)
    } ?: "—"

    val abi = Build.SUPPORTED_ABIS.firstOrNull()
    val device = buildString {
        append(Build.MANUFACTURER).append(' ').append(Build.MODEL)
        if (abi != null) append(" · ").append(abi)
    }

    Surface(
        modifier = Modifier.fillMaxSize(),
        // Opaque, and it has to be — see the class comment. `surface` rather
        // than a named colour so the page follows 主题色 and dark mode with the
        // rest of the app.
        color = MaterialTheme.colorScheme.surface,
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .verticalScroll(rememberScrollState())
                .padding(bottom = 32.dp),
            // The settings list's own rhythm — 12 dp between cards — so the two
            // pages read as the same surface. The gutter is applied per card
            // below rather than here, because `DetourPageHeader` carries its own
            // 20 dp start padding and a second one would double it.
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            DetourPageHeader(
                title = stringResource(R.string.about_title),
                actions = {
                    DetourButton(onClick = onClose, variant = DetourButtonVariant.Text) {
                        Text(stringResource(R.string.about_close))
                    }
                },
            )

            DetourSectionCard(
                title = stringResource(R.string.about_section_version),
                modifier = Modifier.padding(horizontal = 20.dp),
            ) {
                DetourKeyValueRow(
                    label = stringResource(R.string.about_app_version),
                    value = "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})",
                )
                DetourDivider()
                DetourKeyValueRow(
                    label = stringResource(R.string.about_build_type),
                    value = stringResource(
                        if (BuildConfig.DEBUG) {
                            R.string.about_build_debug
                        } else {
                            R.string.about_build_release
                        },
                    ),
                )
                DetourDivider()
                DetourKeyValueRow(
                    label = stringResource(R.string.about_kernel),
                    // The load result, never a version number. The FFI header
                    // exports no version symbol at all, so any number printed
                    // here could only be the *app's* own — which is exactly the
                    // mistake the settings screen's 内核版本 row makes when it
                    // appends `BuildConfig.VERSION_NAME` to "watt-ffi". That row
                    // reports the shell's version under the kernel's name, and a
                    // person reading it while diagnosing a stale `.so` is misled
                    // by it. A null `loadError` means the library is in; the
                    // error string is the whole answer when it is not.
                    value = Kernel.loadError ?: stringResource(R.string.about_kernel_ok),
                )
                // 运行环境 and 设备 live in this card rather than in a section of
                // their own, because there is no section-title string for them —
                // `about_environment` is the *row* label, and using it as a card
                // title as well would print 运行环境 twice, one line apart. The
                // rows themselves are exactly as specified; only the grouping is
                // merged, and "which build, on what runtime" is one subject.
                DetourDivider()
                DetourKeyValueRow(
                    label = stringResource(R.string.about_environment),
                    value = "Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})",
                )
                DetourDivider()
                DetourKeyValueRow(
                    label = stringResource(R.string.about_device),
                    value = device,
                )
            }

            DetourSectionCard(
                title = stringResource(R.string.about_section_rules),
                modifier = Modifier.padding(horizontal = 20.dp),
            ) {
                DetourKeyValueRow(
                    label = stringResource(R.string.about_rule_source),
                    value = sourceLabel,
                )
                DetourDivider()
                DetourKeyValueRow(
                    label = stringResource(R.string.about_rule_scale),
                    value = ruleScale,
                )
            }

            DetourSectionCard(
                title = stringResource(R.string.about_section_update),
                modifier = Modifier.padding(horizontal = 20.dp),
            ) {
                DetourActionRow(
                    label = stringResource(R.string.about_update_url),
                    hint = if (updateUrl.isBlank()) {
                        stringResource(R.string.about_update_unset)
                    } else {
                        updateUrl
                    },
                    actionLabel = stringResource(
                        if (updateUrl.isBlank()) {
                            R.string.about_update_set
                        } else {
                            R.string.about_update_change
                        },
                    ),
                    onAction = { showUrlDialog = true },
                )

                // The 启动时自动检查 switch.
                //
                // **It has to exist.** The startup check is a behaviour that issues a
                // network request on its own: the user taps nothing and the app sends
                // a request to an address. Anything that acts by itself has to be
                // switchable off, or it is not a feature but a nuisance — and once it
                // cannot be turned off, the only means of resistance left to the user
                // is uninstalling. This matches the project's standing rule about
                // controls, read in the other direction: a control must genuinely do
                // something, and a behaviour that genuinely does something must come
                // with a control that genuinely stops it.
                //
                // It sits under 版本清单地址 because the two are halves of one
                // subject: the address decides *where* to check, the switch decides
                // *whether* to check by itself.
                DetourDivider()
                DetourToggleRow(
                    label = stringResource(R.string.about_update_auto_check),
                    hint = stringResource(R.string.about_update_auto_check_hint),
                    checked = prefs.autoCheckUpdate,
                    onChange = { prefs.updateAutoCheckUpdate(it) },
                )

                // The check action exists only once there is an address to check.
                //
                // This is the project's "a control that cannot have an effect
                // must not be shown" rule. With a blank URL, `UpdateChecker.check`
                // returns `Failed("未配置更新地址")` before it opens anything — so
                // the button's only possible outcome is a failure message about
                // the thing the row directly above already says is unset. A button
                // whose every press is a guaranteed error is worse than no button:
                // it invites the press and then blames the user for it.
                if (updateUrl.isNotBlank()) {
                    DetourDivider()
                    Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                        DetourButton(
                            onClick = {
                                // The tap is answered immediately — the label
                                // switches to 正在检查… and the button goes
                                // disabled — before the request starts, and both are
                                // driven by `UpdateState.checking` now that the state
                                // is shared with the startup check.
                                //
                                // The manual check goes through `UpdateState.run` for
                                // the same reason the startup one does — see that
                                // object's class comment: two triggers written twice
                                // would eventually disagree about what "a check is in
                                // flight" means and about when the timestamp is
                                // written. `run` moves the blocking `check` to `IO`
                                // itself, so there is no `withContext` here.
                                scope.launch { UpdateState.run(context) }
                            },
                            // Disabled while in flight, so a second tap cannot
                            // start a second request. The disabled state is also
                            // the only visible sign that the first tap was taken,
                            // since nothing else on the page changes until the
                            // socket answers.
                            enabled = !UpdateState.checking,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text(
                                stringResource(
                                    if (UpdateState.checking) {
                                        R.string.about_update_checking
                                    } else {
                                        R.string.about_update_check
                                    },
                                ),
                            )
                        }
                    }

                    val result = UpdateState.result
                    if (result != null) {
                        DetourDivider()
                        // The three outcomes are rendered as themselves rather
                        // than collapsed into "done": `Failed` in particular must
                        // never look like `UpToDate`, or a wrong URL would read as
                        // a confident "已是最新版本" — see `UpdateChecker`'s own
                        // note on why `Failed` is a first-class result.
                        when (result) {
                            is UpdateChecker.Result.UpToDate -> DetourKeyValueRow(
                                label = stringResource(R.string.about_update_check),
                                value = stringResource(R.string.about_update_latest),
                            )

                            is UpdateChecker.Result.Available -> {
                                // Held in locals so the smart cast to `Available`
                                // is not relied on across the nested lambdas below.
                                val release = result.release
                                DetourKeyValueRow(
                                    label = stringResource(R.string.about_update_check),
                                    value = stringResource(
                                        R.string.about_update_available,
                                        release.versionName,
                                    ),
                                )
                                val notes = release.notes
                                if (!notes.isNullOrBlank()) {
                                    DetourDivider()
                                    Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                                        Text(
                                            stringResource(R.string.about_update_notes),
                                            style = MaterialTheme.typography.labelSmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                        Spacer(Modifier.height(4.dp))
                                        // The upper bound is not optional: this whole
                                        // page scrolls vertically, and without a bound
                                        // the scroll inside `ReleaseNotesText` would
                                        // receive an infinite constraint and throw.
                                        // 240dp is the compromise between "several
                                        // lines visible at a glance" and "does not
                                        // push the download button off the screen";
                                        // anything longer scrolls inside the component.
                                        ReleaseNotesText(
                                            notes = notes,
                                            modifier = Modifier
                                                .fillMaxWidth()
                                                .heightIn(max = 240.dp),
                                        )
                                    }
                                }
                                DetourDivider()
                                Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                                    DetourButton(
                                        onClick = { openInBrowser(context, release.url) },
                                        modifier = Modifier.fillMaxWidth(),
                                    ) {
                                        Text(stringResource(R.string.about_update_download))
                                    }
                                }
                            }

                            is UpdateChecker.Result.Failed -> DetourKeyValueRow(
                                label = stringResource(R.string.about_update_check),
                                value = stringResource(
                                    R.string.about_update_failed,
                                    result.message,
                                ),
                            )
                        }
                    }
                }
            }

            DetourSectionCard(
                title = stringResource(R.string.about_section_what),
                modifier = Modifier.padding(horizontal = 20.dp),
            ) {
                Text(
                    stringResource(R.string.about_description),
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }

    if (showUrlDialog) {
        UpdateUrlDialog(
            initial = updateUrl,
            onDismiss = { showUrlDialog = false },
            onConfirm = { url ->
                prefs.updateUpdateUrl(url)
                showUrlDialog = false
            },
        )
    }
}

/**
 * Hands a URL to whatever the system has registered for `http(s)`.
 *
 * **The browser, deliberately, and not an in-app download.** Installing the APK
 * from here would need `REQUEST_INSTALL_PACKAGES`, a `FileProvider` path for the
 * downloaded file, and the download itself — three pieces of permission surface
 * and a file to keep, for a job the browser already does better and with the
 * user's own consent UI. The owner chose the browser path for exactly that
 * reason: this app's permission list stays small, and there is no code here that
 * writes an installer to the device.
 *
 * `startActivity` is wrapped because a device can genuinely have no browser
 * installed, or none that claims `http`/`https` — an `ActivityNotFoundException`
 * from a tap on a download button would be a crash, and the tap is on a page
 * about version numbers. The failure is logged rather than shown: there is no
 * dialog in this app for "nothing on the device can open a link", and the log
 * page is where a user diagnosing it would look anyway.
 *
 * **This is `internal` rather than `private` because the "a new version is
 * available" dialog needs it too.** That dialog lives in `ReleaseNotes.kt` and is
 * driven by `MainActivity`, so "hand the link to whatever on the system can open
 * it" now has two callers. Copying it next to the dialog would copy the
 * `ActivityNotFoundException` fallback and the log tag as well, and the two copies
 * would diverge the first time only one of them was edited. One function shared
 * within the module is the smaller thing to maintain, and `internal` is enough —
 * it exposes nothing outside.
 */
internal fun openInBrowser(context: Context, url: String) {
    val opened = runCatching {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
    }
    if (opened.isFailure) {
        KernelState.log(
            KernelState.LogEntry.Level.WARN,
            "AboutScreen",
            "没有应用可以打开 $url：${opened.exceptionOrNull()?.message}",
        )
    }
}

/**
 * The version-manifest address editor.
 *
 * Same shape as `SliderInputDialog` in `DetourListItem.kt` — a field, a caption
 * under it, and a confirm that refuses bad input instead of coercing it — so the
 * two dialogs in this app that take typed input behave the same way.
 *
 * **The caption is the hint until the input is rejected, then it is the error.**
 * One line rather than two, because the hint and the error are answering the same
 * question ("what do I put here"), and stacking them would push the field around
 * while the user is reading it.
 */
@Composable
private fun UpdateUrlDialog(
    initial: String,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
) {
    // Seeded with the stored URL, keyed on it so a value that changes underneath
    // (a backup restore, the control channel) re-seeds the field rather than
    // leaving the old text in place — the same reason `SliderInputDialog` keys
    // its `remember` on the incoming value.
    var text by remember(initial) { mutableStateOf(initial) }
    var invalid by remember { mutableStateOf(false) }

    DetourAlertDialog(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.about_update_url_title),
        text = {
            Column {
                OutlinedTextField(
                    value = text,
                    onValueChange = {
                        text = it
                        // Clear the error as soon as the user edits: the message
                        // is about the value that was rejected, and leaving it up
                        // while they retype would be judging input not yet given.
                        invalid = false
                    },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    isError = invalid,
                )
                Spacer(Modifier.height(8.dp))
                Text(
                    stringResource(
                        if (invalid) {
                            R.string.about_update_url_invalid
                        } else {
                            R.string.about_update_url_hint
                        },
                    ),
                    style = MaterialTheme.typography.labelSmall,
                    color = if (invalid) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
            }
        },
        confirmButton = {
            DetourButton(
                onClick = {
                    val trimmed = text.trim()
                    // Blank is accepted, and it is the one case where the field
                    // is not required to look like a URL. The hint above promises
                    // "留空则不检查更新", and blank is a state the whole feature
                    // already understands: it hides the check button and makes
                    // `UpdateChecker.check` refuse before opening a socket. A
                    // validator that rejected it would make the unset state
                    // unreachable through the UI once an address had ever been
                    // set — the only way back to "no update source" would be to
                    // clear the app's data.
                    //
                    // `http://` is refused for the same reason `UpdateChecker`
                    // refuses it, and the two must agree: a dialog that accepted
                    // an address the checker rejects would store a value that
                    // fails only later, at the check button, as the platform's
                    // English cleartext message instead of this hint. The scheme
                    // is not a matter of taste here — the app targets SDK 36 and
                    // grants no cleartext exemption, so `http://` cannot work.
                    //
                    // The scheme test is case-insensitive on purpose.
                    // `UpdateChecker` lowercases the scheme before comparing it,
                    // so `HTTPS://…` is a URL it would accept; rejecting it here
                    // would be this dialog refusing an address the checker is
                    // happy with.
                    val lower = trimmed.lowercase()
                    if (trimmed.isEmpty() || lower.startsWith("https://")) {
                        onConfirm(trimmed)
                    } else {
                        // Keep the dialog open, and say why. Closing it and
                        // storing nothing would look identical to success.
                        invalid = true
                    }
                },
            ) {
                Text(stringResource(R.string.common_confirm))
            }
        },
        dismissButton = {
            DetourButton(onClick = onDismiss, variant = DetourButtonVariant.Text) {
                Text(stringResource(R.string.common_cancel))
            }
        },
    )
}

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
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.rememberCoroutineScope
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
import dev.detour.ui.components.DetourButton
import dev.detour.ui.components.DetourButtonVariant
import dev.detour.ui.components.DetourDivider
import dev.detour.ui.components.DetourKeyValueRow
import dev.detour.ui.components.DetourPageHeader
import dev.detour.ui.components.DetourSectionCard
import dev.detour.ui.components.DetourToggleRow
import dev.detour.ui.components.MarkdownText
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

    // The manifest address is still read here even though this page no longer
    // shows it. It is what decides whether the check button exists at all, and it
    // can still be blank: the control channel's `set update_url ""` and a
    // restored settings document both reach it. The guard below is therefore not
    // dead code even though nothing on this page can clear the value any more.
    val updateUrl = prefs.updateUrl
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
                // The 启动时自动检查 switch, and now the card's first row.
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
                // **The 版本清单地址 row that used to sit above this is gone, and
                // the consequence is accepted rather than overlooked.** That row
                // printed the manifest URL in full — two wrapped lines on every
                // device — and its 修改 button opened a dialog whose only purpose was
                // to point the app at a mirror of the user's own. Nothing about
                // checking for updates requires reading or editing the address, so
                // the row was a permanent piece of developer-facing detail in a
                // user-facing page. What is lost with it is the in-app way to change
                // the source: the default is compiled in
                // (`UpdateChecker.DEFAULT_MANIFEST_URL`), and the only override left
                // is the control channel's `set update_url <url>`.
                //
                // **The https-only rule did not go with the dialog.** It lives in
                // `UpdateChecker.check`, which refuses a non-`https` scheme before
                // opening anything, so an `http://` address written over the control
                // channel still fails with the same message the dialog used to
                // prevent. The dialog was the earlier of two checks, never the only
                // one — which is why deleting it cannot make a bad address reach the
                // network.
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
                // the button's only possible outcome is a failure message about a
                // value this page no longer displays. A button whose every press is
                // a guaranteed error is worse than no button: it invites the press
                // and then blames the user for it.
                //
                // A blank address is still reachable from outside this page — the
                // control channel and a restored settings document can both write
                // one — so the guard is kept rather than dropped along with the
                // address row that used to be able to clear it.
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
                        //
                        // None of the three carries a label. They used to be
                        // key-value rows labelled 检查更新, which printed the
                        // button's own two words a second time one line below the
                        // button. The button is the only thing that can produce
                        // this line, so the label identified nothing — see
                        // `UpdateResultText`.
                        when (result) {
                            is UpdateChecker.Result.UpToDate -> UpdateResultText(
                                stringResource(R.string.about_update_latest),
                            )

                            is UpdateChecker.Result.Available -> {
                                // Held in locals so the smart cast to `Available`
                                // is not relied on across the nested lambdas below.
                                val release = result.release
                                UpdateResultText(
                                    stringResource(
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
                                        // the scroll inside `MarkdownText` would
                                        // receive an infinite constraint and throw.
                                        // 240dp is the compromise between "several
                                        // lines visible at a glance" and "does not
                                        // push the download button off the screen";
                                        // anything longer scrolls inside the component.
                                        MarkdownText(
                                            markdown = notes,
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

                            is UpdateChecker.Result.Failed -> UpdateResultText(
                                stringResource(
                                    R.string.about_update_failed,
                                    result.message,
                                ),
                            )
                        }
                    }
                }
            }
        }
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
 * One outcome line of the update check, with no label.
 *
 * **Why there is no label.** The three outcomes used to be `DetourKeyValueRow`s
 * labelled 检查更新, so the card read:
 *
 *     检查更新                     <- the button's label
 *     --------------------------
 *     检查更新      已是最新版本      <- the result row: the same two words again
 *
 * One line apart, and the label identified nothing the reader did not already
 * know from the line above it. The button is the only thing that can produce
 * this line. On a phone the label also took the left half of the row, which is
 * part of why the address row above it wrapped onto two lines.
 *
 * **It is deliberately not a `DetourKeyValueRow` with an empty label.** That row
 * puts its label in a `Column` carrying `weight(1f)`, so an empty label would
 * leave the value pushed to the right edge of the card, reading as a value whose
 * label failed to load. A plain full-width line is what "no label" means.
 *
 * The padding is the section card's own row rhythm — 16 dp gutter, 12 dp above
 * and below — so this line still lines up with the rows in the cards above it,
 * and the colour is the one `DetourKeyValueRow` uses for its values, so the
 * outcome still reads as a value rather than as a heading.
 */
@Composable
private fun UpdateResultText(text: String) {
    Text(
        text,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

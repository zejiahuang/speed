package dev.detour.ui.components

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import com.mikepenz.markdown.compose.MarkdownSuccess
import com.mikepenz.markdown.m3.Markdown
import com.mikepenz.markdown.m3.markdownTypography

/**
 * A Markdown document, rendered and scrollable.
 *
 * ## What this replaced, and why
 *
 * The app used to flatten Markdown to plain text by deleting markers character by
 * character — `#`, `>`, `**`, backticks — in `releaseNotesPlainText`. That held up
 * while the only document was a GitHub release body. It stopped holding up on the
 * first-launch disclaimer, which is a 23 KB structured document: the flattener had
 * never been taught about inline links or single-asterisk emphasis, so the app
 * showed `[LICENSE](LICENSE)` and `*which address*` — markup, on screen, in a
 * legal notice. The approach could only ever remove the notations someone had
 * remembered to add, which means the next document would leak a new one.
 *
 * So this renders instead of stripping. The parser is `org.jetbrains:markdown` and
 * the styling is the `-m3` artifact's, i.e. the app's own Material 3 theme rather
 * than a palette the library brought with it.
 *
 * ## Why the typography is overridden
 *
 * `markdownTypography()`'s defaults are a **hero scale**: `h1` → `displayLarge`
 * (57sp), `h2` → `displayMedium` (45sp), `h3` → `displaySmall` (36sp). Those sizes
 * belong on a full-screen landing page. Every caller of this component puts it in
 * a bounded box instead — a dialog roughly 280dp wide, or a 240dp-tall slot on the
 * About page — and the disclaimer is 30 `##` headings deep. At the default scale
 * `## 一、总则与适用` is wider than the dialog and gets clipped mid-glyph, and the
 * body reads as a wall of display type. Nothing is wrong with the scale in the
 * abstract; it is wrong here, at every call site.
 *
 * The headings are **not bolded by the library** — `MarkdownHeader` passes the
 * `TextStyle` through verbatim — so the weight has to be set here too. A heading
 * that differs from the body only in size reads as a layout accident.
 *
 * The body is `bodyMedium` (14sp) rather than the default `bodyLarge` (16sp):
 * 14sp is what Material 3's own `AlertDialog` uses for its text, and it is what
 * keeps `h3` (14sp) from being *smaller* than the paragraph under it. Two of the
 * three call sites are dialogs; the third is a fixed-height slot on a page.
 *
 * ## Why there is a spinner, and why `success` is overridden
 *
 * `Markdown(content = …)` does not render synchronously. Its state starts at
 * `State.Loading` and its **default loading slot is an empty `Box`** — so the
 * first frame of the first-launch disclaimer is a blank panel where a legal
 * document should be. How long that lasts is a property of the build, not of the
 * document: on the emulator the body stayed blank for about **four seconds** in a
 * debug build, where the renderer's classes are loaded and verified at first use,
 * while in a release build the content was already there on the first frame the
 * screenshot could catch. Four seconds of an empty box inside a consent gate reads
 * as a broken gate, not as a slow one, and the debug build is the one this app is
 * developed against. A spinner is the same wait with the right label on it.
 *
 * The `success` slot is overridden only to move `verticalScroll` off the shared
 * modifier and onto the rendered column. `Markdown` hands **one** modifier to all
 * three slots, so a scroll modifier passed to the component itself would also be
 * applied to the loading and error boxes — and `verticalScroll` measures its child
 * with an unbounded height, which would make the loading box collapse to the
 * spinner's own size and let the dialog resize when the text arrives. Scrolling
 * only the success column leaves the placeholder free to fill the height the
 * caller reserved, so the dialog is the same size before and after the swap.
 *
 * ## The height contract, unchanged from the component it replaces
 *
 * **The caller must give this a bounded height (`heightIn` / `height`); it is not
 * optional.** The component scrolls internally, and when its parent is itself a
 * vertical scroll container — which the whole About page is — the child receives
 * an infinite maximum height, and `verticalScroll` throws on an infinite
 * constraint. With an upper bound the overflow scrolls inside this component and
 * the page does not have to make room for a body of unpredictable length.
 *
 * That bound is also what lets the loading box use `fillMaxHeight()`: the
 * constraint arriving from the caller is finite by contract, so the placeholder
 * occupies exactly the space the document will.
 *
 * The modifier is passed straight through to `Markdown`, whose own default is
 * `Modifier.fillMaxSize()` — that default is why a caller's size constraint has to
 * be forwarded rather than dropped.
 *
 * **Parsing happens off the main thread and is owned by the library.**
 * `rememberMarkdownState` parses on `Dispatchers.Default`, which is why the
 * disclaimer does not stall the frame that shows it. What it cannot move is the
 * one-time cost of loading the renderer's classes.
 */
@Composable
fun MarkdownText(markdown: String, modifier: Modifier = Modifier) {
    Markdown(
        content = markdown,
        typography = markdownTypography(
            h1 = MaterialTheme.typography.titleLarge.copy(fontWeight = FontWeight.Bold),
            h2 = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.Bold),
            h3 = MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.Bold),
            h4 = MaterialTheme.typography.labelLarge.copy(fontWeight = FontWeight.Bold),
            h5 = MaterialTheme.typography.labelMedium.copy(fontWeight = FontWeight.Bold),
            h6 = MaterialTheme.typography.labelMedium.copy(fontWeight = FontWeight.Bold),
            text = MaterialTheme.typography.bodyMedium,
            paragraph = MaterialTheme.typography.bodyMedium,
            ordered = MaterialTheme.typography.bodyMedium,
            bullet = MaterialTheme.typography.bodyMedium,
            list = MaterialTheme.typography.bodyMedium,
        ),
        modifier = modifier,
        loading = { inner ->
            Box(
                modifier = inner.fillMaxWidth().fillMaxHeight(),
                contentAlignment = Alignment.Center,
            ) {
                CircularProgressIndicator()
            }
        },
        success = { state, components, inner ->
            MarkdownSuccess(
                state = state,
                components = components,
                modifier = inner.verticalScroll(rememberScrollState()),
            )
        },
    )
}

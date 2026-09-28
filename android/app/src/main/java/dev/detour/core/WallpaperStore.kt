package dev.detour.core

import android.content.Context
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Log
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest

/**
 * The user's background photo, copied into app storage and made identifiable.
 *
 * ## The bytes are copied; the `content://` URI is not kept
 *
 * The photo picker hands back a `content://` URI whose read grant is **one-shot**:
 * it lasts for the process that received the result and does not survive a
 * reboot or a task death. A wallpaper that is "the URI the user picked" therefore
 * works until the next launch and is blank after it — the worst kind of failure,
 * because it only appears once the feature looks finished. So [save] streams the
 * image into `filesDir` immediately, while the grant is still live, and every
 * later read is from our own file. This is the single most important design
 * constraint here.
 *
 * ## The stored image is identified, not merely present
 *
 * `RulesRepository` records a hard-won lesson in this repo: a cache validated
 * only by age was reused after an upgrade even though it held a different rule
 * set, and the result was a silent "nothing works". The same shape of bug is
 * available here — a pref that names one photo while `filesDir` holds another
 * (left behind by an upgrade, a half-finished write, or a restore-to-defaults
 * that cleared the pref but not the file) would show the wrong background with
 * nothing to indicate it. So the bytes and their **identity** are written as a
 * pair: [IMAGE_NAME] holds the image, and a sidecar ([META_NAME]) records a
 * SHA-256 of exactly those bytes plus their size and mime type. A read requires
 * the pref's identity and the sidecar's identity to agree, and the file's length
 * to match the sidecar's byte count; anything else is treated as "no wallpaper"
 * rather than served.
 *
 * ## Degrade, never throw
 *
 * A missing file, a truncated file, a decode failure, an unreadable sidecar, or a
 * pref/disk mismatch all resolve to `null`. [load] is called from a composable,
 * so it must never throw and must never return a partially-decoded bitmap — a
 * blank or default background is always preferable to a crash on the home screen.
 *
 * ## Downsampling is not optional
 *
 * A 4000×3000 JPEG decodes to roughly 48 MB at ARGB_8888. The target emulator is a
 * 2-core low-RAM device, so a full-size decode is a genuine OOM risk, not a
 * theoretical one. [load] therefore reads the header first, computes an
 * `inSampleSize` for the display, and decodes at that reduced size — a wallpaper
 * only ever fills the screen, so the extra pixels buy nothing and cost memory.
 */
object WallpaperStore {

    private const val TAG = "DetourWallpaper"

    /** The copied image bytes. The extension is deliberately format-neutral. */
    private const val IMAGE_NAME = "wallpaper.img"

    /**
     * Records what [IMAGE_NAME] holds: identity (SHA-256), byte count, mime type
     * and the source dimensions.
     *
     * A sidecar rather than a header inside the image, for the same reason
     * `RulesRepository` keeps `rules.json.source` apart from `rules.json`: the
     * image is an opaque file that must stay byte-for-byte what the user picked,
     * and a stamp bolted onto it would make it no longer that file. The image is
     * written first and the sidecar second, so a crash between the two leaves a
     * bare image that reads as "no wallpaper" and is refetched — a wasted pick is
     * the cheap side of that trade.
     */
    private const val META_NAME = "wallpaper.img.meta"

    /**
     * Copy the picked photo into app storage and make it the current wallpaper.
     *
     * Returns the identity that was stored, or `null` when nothing usable could
     * be copied — an unreadable URI, an empty stream, or bytes that are not a
     * decodable image. `null` means "the wallpaper did not change", so a caller
     * can surface a failure without having to reason about partial state. On
     * success this also writes the identity through to `Prefs`, so the file, the
     * sidecar and the pref are updated together.
     *
     * Runs on [Dispatchers.IO]: it reads the whole image off the picker's stream.
     */
    suspend fun save(context: Context, uri: Uri): String? = withContext(Dispatchers.IO) {
        runCatching { saveBlocking(context, uri) }.getOrNull()
    }

    /**
     * Forget the wallpaper: clear the pref, delete the image and its sidecar, and
     * put the scrim back to its default.
     *
     * The two files are one fact stored in two places, so they are always removed
     * together — a delete that left one behind would leave a sidecar vouching for
     * an image that is gone, which is exactly the pairing the identity check
     * exists to catch.
     *
     * **The scrim is reset here, and that is not tidiness.** The scrim is not a
     * standalone display setting; it is a property of a particular photo, and it
     * only means anything while that photo is on screen. Leaving it behind is the
     * `RulesRepository` mistake in miniature — a stored value outliving the thing
     * it described — and it fails in the worst way: clear the wallpaper, drag the
     * scrim to 100% at some point, then pick a *new* photo, and the new photo
     * renders behind a 100% scrim. The screen goes flat white and the pick looks
     * like it did nothing at all, which is exactly the "the control does nothing"
     * defect this project has already been burned by twice. Measured on the
     * emulator before this fix: `wallpaper_scrim=99` survived a clear, and the
     * next photo was invisible behind it.
     *
     * Resetting to the default rather than to zero is deliberate: zero is a
     * legitimate choice a user can make, so it must not be a value the app
     * imposes, and the default is the one value known to be visible.
     */
    fun clear(context: Context) {
        runCatching { Prefs.of(context).updateWallpaper("") }
        runCatching { Prefs.of(context).updateWallpaperScrim(Prefs.DEFAULT_WALLPAPER_SCRIM) }
        discard(context)
    }

    /**
     * Decode the current wallpaper for display, or `null` when there is none.
     *
     * Safe to call from a `LaunchedEffect`: it never throws, runs on
     * [Dispatchers.IO], and returns a bitmap already downsampled to the display
     * size (see the class comment for why that matters). Any failure — no
     * wallpaper set, missing file, truncated file, unreadable sidecar, a
     * pref/disk identity mismatch, or a decode/OOM failure — yields `null`.
     */
    suspend fun load(context: Context): ImageBitmap? = withContext(Dispatchers.IO) {
        runCatching { loadBlocking(context) }.getOrNull()
    }

    /** The image file, for callers that need the path (e.g. a "clear" affordance). */
    fun imageFile(context: Context): File = File(context.filesDir, IMAGE_NAME)

    /** The sidecar that records what [imageFile] holds. */
    fun metaFile(context: Context): File = File(context.filesDir, META_NAME)

    // --- internals -----------------------------------------------------------

    private fun saveBlocking(context: Context, uri: Uri): String? {
        val image = imageFile(context)
        val meta = metaFile(context)
        // Copy into a temp file first so a failure part-way through cannot leave a
        // half-written image where a complete one used to be.
        val temp = File(context.filesDir, "$IMAGE_NAME.tmp")
        runCatching { temp.delete() }

        val digest = MessageDigest.getInstance("SHA-256")
        val byteCount = try {
            context.contentResolver.openInputStream(uri)?.use { input ->
                temp.outputStream().use { output ->
                    val buffer = ByteArray(64 * 1024)
                    var total = 0L
                    while (true) {
                        val read = input.read(buffer)
                        if (read < 0) break
                        output.write(buffer, 0, read)
                        digest.update(buffer, 0, read)
                        total += read
                    }
                    total
                }
            } ?: return null
        } catch (err: Throwable) {
            Log.w(TAG, "wallpaper copy failed", err)
            runCatching { temp.delete() }
            return null
        }
        if (byteCount <= 0L) {
            runCatching { temp.delete() }
            return null
        }

        // Reject undecodable bytes *before* they are stored: a header-only decode
        // is cheap, and it is the only point at which we can tell the user the
        // pick failed instead of silently storing a background that will never
        // render. A file whose dimensions cannot be read is not an image.
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(temp.absolutePath, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) {
            runCatching { temp.delete() }
            return null
        }

        // Replace the previous image. `renameTo` within `filesDir` is a cheap
        // atomic swap; if the filesystem refuses it, fall back to a copy.
        runCatching { image.delete() }
        if (!temp.renameTo(image)) {
            runCatching { temp.copyTo(image, overwrite = true) }
            runCatching { temp.delete() }
        }

        val identity = digest.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) }
        val mime = runCatching { context.contentResolver.getType(uri) }.getOrNull()
            ?: "application/octet-stream"
        val metaJson = JSONObject()
            .put("identity", identity)
            .put("mime", mime)
            .put("bytes", byteCount)
            .put("width", bounds.outWidth)
            .put("height", bounds.outHeight)
            .put("savedAt", System.currentTimeMillis())

        return try {
            meta.writeText(metaJson.toString())
            Prefs.of(context).updateWallpaper(identity)
            KernelState.log(
                KernelState.LogEntry.Level.INFO, TAG,
                "壁纸已保存（${mime}，${byteCount / 1024} KiB，" +
                    "${bounds.outWidth}×${bounds.outHeight}）",
            )
            identity
        } catch (err: Throwable) {
            // The image is on disk but nothing vouches for it. Remove it rather
            // than leave an unidentifiable file behind.
            Log.w(TAG, "wallpaper sidecar write failed", err)
            discard(context)
            null
        }
    }

    private fun loadBlocking(context: Context): ImageBitmap? {
        val identity = Prefs.of(context).wallpaper
        val image = imageFile(context)

        if (identity.isBlank()) {
            // Nothing is selected, so any bytes left in `filesDir` are an orphan:
            // `restoreDefaults()` clears the pref but `Prefs` has no file I/O by
            // design, and a reinstall can keep app storage. Pruning here is what
            // keeps the pref and the disk from drifting apart.
            discard(context)
            return null
        }

        val recorded = readMeta(context)
        // "Healthy" is three facts, not one: the sidecar is readable, it names the
        // same photo the pref does, and the file on disk is exactly as long as the
        // sidecar promises. Any one of them being false means the bytes cannot be
        // vouched for.
        val healthy = recorded != null &&
            recorded.identity == identity &&
            image.isFile &&
            image.length() > 0L &&
            image.length() == recorded.bytes
        if (!healthy) {
            // The pref names a wallpaper the disk cannot vouch for — a truncated
            // write, a stale file from an upgrade, or a pref/disk mismatch. Rather
            // than serve an image we cannot identify, forget it entirely so the UI
            // honestly shows "no wallpaper" and the next pick starts clean. This is
            // the failure the `RulesRepository` lesson warns about, and refusing is
            // the same answer it arrived at.
            clear(context)
            return null
        }
        return decode(context, image)
    }

    /**
     * Decode [image] at an `inSampleSize` chosen for the current display.
     *
     * Two passes: a bounds-only decode to learn the real dimensions without
     * allocating pixels, then the real decode at the reduced size. See the class
     * comment for why a full-size decode is an OOM risk on the target device.
     */
    private fun decode(context: Context, image: File): ImageBitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(image.absolutePath, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null

        val metrics = context.resources.displayMetrics
        val options = BitmapFactory.Options().apply {
            inSampleSize = sampleSize(
                bounds.outWidth,
                bounds.outHeight,
                metrics.widthPixels.coerceAtLeast(1),
                metrics.heightPixels.coerceAtLeast(1),
            )
        }
        // `decodeFile` returns null on a corrupt/truncated image rather than a
        // partial bitmap, and the caller's `runCatching` turns an OutOfMemoryError
        // on this allocation into `null` as well. Either way: no wallpaper, no crash.
        val bitmap = BitmapFactory.decodeFile(image.absolutePath, options) ?: return null
        return bitmap.asImageBitmap()
    }

    /**
     * The largest power-of-two reduction that still covers the target size.
     *
     * Never returns less than 1: downsampling exists to save memory, and a bitmap
     * already smaller than the display should be decoded as-is rather than
     * upscaled.
     */
    private fun sampleSize(width: Int, height: Int, reqWidth: Int, reqHeight: Int): Int {
        var sample = 1
        var w = width
        var h = height
        while (w / 2 >= reqWidth && h / 2 >= reqHeight) {
            w /= 2
            h /= 2
            sample *= 2
        }
        return sample
    }

    /** The sidecar's contents, or `null` if it is missing or unreadable. */
    private fun readMeta(context: Context): Meta? = runCatching {
        val file = metaFile(context)
        if (!file.isFile) return@runCatching null
        val json = JSONObject(file.readText())
        val identity = json.optString("identity")
        if (identity.isBlank()) return@runCatching null
        Meta(
            identity = identity,
            bytes = json.optLong("bytes", -1L),
        )
    }.getOrNull()

    /** Delete the image, its sidecar, and any leftover temp file, together. */
    private fun discard(context: Context) {
        runCatching { imageFile(context).delete() }
        runCatching { metaFile(context).delete() }
        runCatching { File(context.filesDir, "$IMAGE_NAME.tmp").delete() }
    }

    /** What the sidecar records about the stored image. */
    private data class Meta(val identity: String, val bytes: Long)
}

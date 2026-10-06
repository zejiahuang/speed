package dev.detour.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.detour.R
import dev.detour.ui.theme.DetourTheme

/**
 * The row vocabulary the settings screen is built from.
 *
 * These are the rows that used to be private to `SettingsScreen.kt`. Lifting
 * them out is the whole reason the settings page can be a list of data rather
 * than a wall of layout: a row is "a label, an optional hint and a control", and
 * there is now exactly one implementation of that.
 *
 * The components hold no state and read no `Prefs` — a row is given a value and
 * an `onChange`, which is what makes each of them previewable in isolation.
 */

/** A titled group of rows. */
@Composable
fun DetourSectionCard(
    title: String,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(modifier) {
        Text(
            title,
            modifier = Modifier.padding(start = 4.dp, bottom = 8.dp),
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
        )
        // Glass, and deliberately without a `colors` argument. This one call is
        // the app's only group-card shape, so the switch here puts the frosted
        // surface on every settings group *and* on the home screen's 规则状态 /
        // 命中规则 cards at once. No `containerColor` is passed because
        // `LiquidGlassSurface` supplies its own tint — a colour here would be an
        // argument nothing reads, which is worse than no argument at all.
        DetourCard(style = DetourCardStyle.Glass) {
            Column(Modifier.padding(vertical = 4.dp), content = content)
        }
    }
}

/** The hairline between two rows of a section. */
@Composable
fun DetourDivider(modifier: Modifier = Modifier) {
    HorizontalDivider(
        modifier = modifier.padding(horizontal = 16.dp),
        color = MaterialTheme.colorScheme.outlineVariant,
    )
}

/** Label + optional hint on the left, a control on the right. */
@Composable
private fun DetourRow(
    label: String,
    hint: String?,
    modifier: Modifier = Modifier,
    trailing: @Composable () -> Unit,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyLarge)
            if (hint != null) {
                Text(
                    hint,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        trailing()
    }
}

/** A switch row. */
@Composable
fun DetourToggleRow(
    label: String,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    hint: String? = null,
    enabled: Boolean = true,
) {
    DetourRow(label, hint, modifier) {
        DetourSwitch(checked = checked, onCheckedChange = onChange, enabled = enabled)
    }
}

/** A `− value +` row for a bounded integer. */
@Composable
fun DetourStepperRow(
    label: String,
    value: String,
    onDecrease: () -> Unit,
    onIncrease: () -> Unit,
    modifier: Modifier = Modifier,
    hint: String? = null,
) {
    DetourRow(label, hint, modifier) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            TextButton(onClick = onDecrease) { Text("−") }
            Text(value, style = MaterialTheme.typography.titleMedium)
            TextButton(onClick = onIncrease) { Text("+") }
        }
    }
}

/**
 * A slider row, with the value shown at the right of the label.
 *
 * **The `remember(value)` key is load-bearing.** `remember { mutableFloatStateOf(value) }`
 * captures the first value and ignores every later one, so a slider restored
 * from a backup or moved by the control channel would keep showing the old
 * position while the setting had already changed. Keying the `remember` on the
 * incoming value re-seeds the drag state when the source of truth moves.
 *
 * **The value text is tappable, and that is the only way to reach a number the
 * thumb cannot land on.** A drag can only ever approximate: with a 0..150 range
 * across a phone width, one pixel of travel is several units, so "exactly 100"
 * is not reachable by thumb at all. Typing is the fix, and it lives here rather
 * than in the glass rows because *every* slider in the app has the same
 * unreachable-exact-value problem — one implementation at the shared row means
 * all eleven call sites get it, and none can drift.
 */
@Composable
fun DetourSliderRow(
    label: String,
    value: Float,
    range: ClosedFloatingPointRange<Float>,
    display: String,
    onChange: (Float) -> Unit,
    modifier: Modifier = Modifier,
) {
    var local by remember(value) { mutableFloatStateOf(value) }
    var editing by remember { mutableStateOf(false) }
    Column(modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(label, style = MaterialTheme.typography.bodyLarge)
            Text(
                display,
                modifier = Modifier.clickable { editing = true },
                style = MaterialTheme.typography.titleMedium,
            )
        }
        DetourSlider(
            value = local,
            onValueChange = { local = it },
            valueRange = range,
            onValueChangeFinished = { onChange(local) },
        )
    }

    if (editing) {
        SliderInputDialog(
            initial = value,
            range = range,
            onDismiss = { editing = false },
            onConfirm = { typed ->
                // `local` is set as well as reported, and that is not redundant.
                // If the typed number coerces back to the value already stored —
                // or is simply the same number — `onChange` produces no state
                // change, so `remember(value)` never re-seeds and the thumb would
                // stay where it was. The typed value would appear to be discarded
                // by a control that visibly did nothing. Writing `local` first
                // moves the thumb regardless.
                local = typed
                onChange(typed)
                editing = false
            },
        )
    }
}

/**
 * The numeric-entry dialog behind a tappable slider value.
 *
 * **Seeded with the raw number, never with `display`.** `display` is a
 * presentation string — "16dp", "72%", "0.85" — and a field seeded with a unit
 * suffix would fail `toFloatOrNull()` on the very first confirm, so the user's
 * only working move would be to clear the field first. The raw `Float` has no
 * such problem; it is rendered as a plain integer when it is whole (1.0 → "1")
 * and otherwise as its own string (0.85 → "0.85"), which is exactly what a
 * person would type back.
 *
 * `KeyboardType.Decimal` rather than `Number` because one of the sliders
 * (`fontScale`) is fractional and its whole point is 0.85 and 1.3. An integer
 * slider loses nothing from a decimal keypad — "16" types fine — so one keyboard
 * serves both, and a per-slider switch would be a degree of freedom nothing
 * needs.
 */
@Composable
private fun SliderInputDialog(
    initial: Float,
    range: ClosedFloatingPointRange<Float>,
    onDismiss: () -> Unit,
    onConfirm: (Float) -> Unit,
) {
    val seed = if (initial == initial.toInt().toFloat()) {
        initial.toInt().toString()
    } else {
        initial.toString()
    }
    var text by remember(seed) { mutableStateOf(seed) }
    DetourAlertDialog(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.slider_input_title),
        text = {
            Column {
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    modifier = Modifier.fillMaxWidth(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                    singleLine = true,
                )
                Spacer(Modifier.height(8.dp))
                Text(
                    stringResource(R.string.slider_input_range, range.start, range.endInclusive),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = {
            DetourButton(
                onClick = {
                    // Unparseable input is ignored rather than clamped to an end:
                    // silently turning a typo into 0 or 150 would be a value the
                    // user did not ask for, and the dialog simply staying open is
                    // the honest response to "that is not a number".
                    text.toFloatOrNull()?.coerceIn(range)?.let(onConfirm)
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

/**
 * A row of chips, one selected.
 *
 * Kept for four or more options. For two or three, [DetourSegmentedRow] is the
 * better control — a segmented button reads as "pick one of these few" and a
 * chip row reads as "filter", which is why the settings screen uses each where
 * it fits rather than one everywhere.
 *
 * **The chip strip scrolls, and that is the fix for a real defect rather than
 * polish.** This was a plain `Row`: with five options on a 1600×900 screen the
 * last chip was the one squeezed, and its label wrapped to one character per
 * line — 紫罗兰 rendered as three stacked glyphs in a chip stretched taller than
 * its neighbours. A `Row` does not scroll, it compresses, so the overflow always
 * lands on the last child. `horizontalScroll` gives the overflow somewhere to
 * go instead.
 *
 * `weight(1f)` on each chip is the obvious-looking alternative and it is wrong:
 * splitting the width five ways still leaves each chip narrower than its label
 * once the labels are two or three CJK glyphs, so the wrap returns, only now on
 * every chip at once. The strip has to be allowed to be wider than the screen.
 *
 * This is the same treatment the rule page's source row already uses — see
 * `RulesScreen.kt`'s source-chip row, whose comment records the identical
 * reasoning ("a plain row would push the button off the edge"). Two rows with
 * the same failure mode now have the same answer.
 *
 * [hint] exists for the same reason [DetourSegmentedRow] has one: a setting
 * whose meaning is not obvious from its options needs a line of explanation, and
 * folding it into the label makes the label long enough that the chips below it
 * stop reading as its answer.
 */
@Composable
fun DetourChoiceRow(
    label: String,
    options: List<Pair<String, Int>>,
    selected: String,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
    hint: String? = null,
) {
    Column(modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
        Text(label, style = MaterialTheme.typography.bodyLarge)
        if (hint != null) {
            Text(
                hint,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.height(8.dp))
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            options.forEach { (value, labelRes) ->
                DetourFilterChip(
                    selected = selected == value,
                    onClick = { onSelect(value) },
                    label = stringResource(labelRes),
                )
            }
        }
    }
}

/** A read-only label/value row. */
@Composable
fun DetourKeyValueRow(
    label: String,
    value: String,
    modifier: Modifier = Modifier,
) {
    DetourRow(label, null, modifier) {
        Text(
            value,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * A label and optional hint on the left, one or two text actions on the right.
 *
 * The row vocabulary had no "action" shape: a row could toggle, step, slide or
 * choose, but there was no way to say "this row does a thing" — which is what
 * the wallpaper row needs, since picking a photo is neither a value nor a
 * selection. [DetourRow] is reused rather than re-implemented so the label and
 * hint land in exactly the same place as every other row.
 *
 * The second action is optional because "clear" only means something once a
 * wallpaper is set: before a pick there is nothing to clear, and a disabled
 * button that can never do anything is worse than no button.
 */
@Composable
fun DetourActionRow(
    label: String,
    actionLabel: String,
    onAction: () -> Unit,
    modifier: Modifier = Modifier,
    hint: String? = null,
    secondaryLabel: String? = null,
    onSecondary: (() -> Unit)? = null,
) {
    DetourRow(label, hint, modifier) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            TextButton(onClick = onAction) { Text(actionLabel) }
            if (secondaryLabel != null && onSecondary != null) {
                TextButton(onClick = onSecondary) { Text(secondaryLabel) }
            }
        }
    }
}

/**
 * A row that opens another page.
 *
 * **The whole row is the tap target, and the trailing edge carries a chevron
 * instead of a labelled button.** It was a [DetourActionRow] whose action read
 * 查看, which said the wrong thing twice: a text button at the right of a row
 * that navigates splits the row into "a label you read" and "a button you
 * press", and it invites the reader to aim at the button when the thing being
 * chosen is the row. A chevron is the convention for "this goes somewhere", and
 * it is not a button, so the row is what answers the tap.
 *
 * `KeyboardArrowRight` is the only chevron in `material-icons-core`, which is
 * the only icon artifact this app ships — see the note on the second 添加自定义源
 * row in `SettingsScreen`, and the build file. `AutoMirrored` because a chevron
 * is a direction: in a right-to-left layout the row still opens forwards, so the
 * glyph has to point the other way.
 *
 * The icon is decorative and carries no `contentDescription`: the label already
 * names the destination, and describing the chevron as well would have a screen
 * reader announce it twice.
 *
 * [hint] is the second line of the row — the same one-sentence answer to "what
 * is behind this" that every other row's hint gives.
 */
@Composable
fun DetourNavRow(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    hint: String? = null,
) {
    DetourRow(
        label = label,
        hint = hint,
        // Applied before `DetourRow`'s own padding, so the ripple covers the
        // whole row rather than stopping at the label's edge — the tap target
        // and the visible row are the same rectangle.
        modifier = modifier.clickable(onClick = onClick),
    ) {
        Icon(
            imageVector = Icons.AutoMirrored.Filled.KeyboardArrowRight,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Preview
@Composable
private fun DetourListItemPreview() {
    DetourTheme {
        DetourSectionCard(title = "示例分组") {
            DetourToggleRow(label = "开关", checked = true, onChange = {})
            DetourDivider()
            DetourStepperRow(label = "步进", value = "3", onDecrease = {}, onIncrease = {})
            DetourDivider()
            DetourSliderRow(
                label = "滑块",
                value = 0.5f,
                range = 0f..1f,
                display = "50%",
                onChange = {},
            )
            DetourDivider()
            DetourKeyValueRow(label = "版本", value = "watt-ffi 0.1.0")
            DetourDivider()
            DetourActionRow(
                label = "背景壁纸",
                hint = "自定义应用背景",
                actionLabel = "选择图片",
                onAction = {},
                secondaryLabel = "清除",
                onSecondary = {},
            )
            DetourDivider()
            DetourNavRow(label = "数据与关于", hint = "导出、导入与版本信息", onClick = {})
        }
    }
}

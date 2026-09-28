package dev.detour.ui.components

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import dev.detour.ui.theme.DetourTheme

/**
 * The one dialog.
 *
 * A thin wrapper, but it fixes the two things a raw `AlertDialog` call site kept
 * getting subtly wrong: the title is always a `Text` at the theme's title style,
 * and the body is a nullable slot so a confirmation with no body does not need a
 * fake empty `text = {}`.
 */
@Composable
fun DetourAlertDialog(
    onDismissRequest: () -> Unit,
    title: String,
    confirmButton: @Composable () -> Unit,
    modifier: Modifier = Modifier,
    text: (@Composable () -> Unit)? = null,
    dismissButton: (@Composable () -> Unit)? = null,
) {
    AlertDialog(
        onDismissRequest = onDismissRequest,
        modifier = modifier,
        title = { Text(title) },
        text = text,
        confirmButton = confirmButton,
        dismissButton = dismissButton,
    )
}

@Preview
@Composable
private fun DetourDialogPreview() {
    DetourTheme {
        DetourAlertDialog(
            onDismissRequest = {},
            title = "标题",
            text = { Text("正文") },
            confirmButton = { DetourButton(onClick = {}) { Text("确定") } },
        )
    }
}

package dev.detour.ui

import androidx.compose.ui.unit.dp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The power control's halo is 8/7 of the circle, so sizing the circle against
 * `maxHeight - reserve` overflows and Compose clips the last child — the status
 * line. That is how "Connected" became a one-pixel sliver on a 720×1280 screen
 * while the simulator showed it as connected. These cases pin the arithmetic:
 * whatever the circle ends up being, the halo plus the reserve has to fit.
 */
class HeroLayoutTest {
    @Test
    fun `the halo and the status line fit in the available height`() {
        val reserve = 56.dp
        // 166dp is the tightest height that still clears the 96dp minimum.
        for (height in listOf(166.dp, 170.dp, 195.dp, 240.dp, 320.dp)) {
            val diameter = heroDiameterAt(height, reference = 256.dp, reserve = reserve)
            val occupied = diameter * (8f / 7f) + reserve
            assertTrue(
                "height $height: circle $diameter occupies $occupied",
                occupied <= height + 0.5.dp,
            )
        }
    }

    @Test
    fun `the reference diameter is a ceiling`() {
        assertEquals(128.dp, heroDiameterAt(400.dp, reference = 128.dp, reserve = 56.dp))
    }

    @Test
    fun `the control never collapses below its minimum`() {
        assertEquals(96.dp, heroDiameterAt(120.dp, reference = 256.dp, reserve = 56.dp))
    }
}

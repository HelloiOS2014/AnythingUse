package dev.anythinguse.lau.helper

import android.view.accessibility.AccessibilityNodeInfo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * JVM unit tests for the helper's pure rules (plan §0 #14). Run with:
 *   ./native/android-helper/<gradle> :app:testDebugUnitTest
 * or simply `./scripts/test-android-helper.sh`.
 */
class PureRulesTest {

    @Test
    fun onlyRootAndShellMayTalkToTheHelper() {
        // Plan §4: adb forward arrives as shell; device-local apps must be refused.
        assertTrue(PureRules.peerUidAllowed(PureRules.ROOT_UID))
        assertTrue(PureRules.peerUidAllowed(PureRules.SHELL_UID))
        assertFalse(PureRules.peerUidAllowed(10123)) // a normal app
        assertFalse(PureRules.peerUidAllowed(1000)) // system
        // Unreadable credentials must not break the working path.
        assertTrue(PureRules.peerUidAllowed(null))
    }

    @Test
    fun role_isShortenedForTheObservation() {
        assertEquals("Button", PureRules.shortRole("android.widget.Button"))
        assertEquals("EditText", PureRules.shortRole("android.widget.EditText"))
        assertEquals("View", PureRules.shortRole(null))
        assertEquals("View", PureRules.shortRole(""))
        assertEquals("View", PureRules.shortRole("   "))
        // A class without a package keeps its name.
        assertEquals("Custom", PureRules.shortRole("Custom"))
    }

    @Test
    fun scroll_takesOnlyTheDominantAxisAndItsSign() {
        assertEquals(
            AccessibilityNodeInfo.ACTION_SCROLL_FORWARD,
            PureRules.scrollAction(0.0, 1.0),
        )
        assertEquals(
            AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD,
            PureRules.scrollAction(0.0, -1.0),
        )
        // Horizontal when it dominates.
        assertEquals(
            AccessibilityNodeInfo.ACTION_SCROLL_FORWARD,
            PureRules.scrollAction(1.0, 0.2),
        )
        assertEquals(
            AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD,
            PureRules.scrollAction(-1.0, 0.2),
        )
        // Magnitude is never interpreted: only axis + sign.
        assertEquals(
            PureRules.scrollAction(0.0, 0.01),
            PureRules.scrollAction(0.0, 99.0),
        )
        // A zero delta is not a scroll.
        assertNull(PureRules.scrollAction(0.0, 0.0))
    }
}

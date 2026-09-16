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
    fun aCredentialIsFlaggedNeverRead() {
        // Plan §5.4/§5.6 + privacy: the observation may carry the fact that a
        // field is a credential, never the credential itself.
        assertNull(PureRules.observationValue(isEditable = true, isPassword = true, text = "hunter2"))
        assertEquals(
            "hunter2",
            PureRules.observationValue(isEditable = true, isPassword = false, text = "hunter2"),
        )
        // Not editable → nothing to report even when text exists.
        assertNull(PureRules.observationValue(isEditable = false, isPassword = false, text = "x"))
        assertNull(PureRules.observationValue(isEditable = true, isPassword = false, text = null))
    }

    @Test
    fun aCredentialContributesNoLabelText() {
        // The sneakier leak: a password box's text becoming a row's label.
        assertEquals(
            emptyList<String>(),
            PureRules.labelContribution(text = "hunter2", contentDescription = null, isPassword = true),
        )
        // Its hint (contentDescription) is still useful and safe.
        assertEquals(
            listOf("密码"),
            PureRules.labelContribution(text = "hunter2", contentDescription = "密码", isPassword = true),
        )
        // A normal field contributes its text, with the description second.
        assertEquals(
            listOf("WLAN", "WLAN 设置"),
            PureRules.labelContribution(text = "WLAN", contentDescription = "WLAN 设置", isPassword = false),
        )
    }

    @Test
    fun capabilitiesAreOnlyWhatTheNodeOffers() {
        // A read-only label advertises nothing.
        assertEquals(
            emptyList<String>(),
            PureRules.capabilities(
                clickable = false, hasClickAction = false, editable = false,
                hasSetTextAction = false, focusable = false, hasFocusAction = false,
                scrollable = false, hasScrollActions = false,
            ),
        )
        // A clickable row: invoke + focus.
        assertEquals(
            listOf("invoke", "focus"),
            PureRules.capabilities(
                clickable = true, hasClickAction = true, editable = false,
                hasSetTextAction = false, focusable = true, hasFocusAction = true,
                scrollable = false, hasScrollActions = false,
            ),
        )
        // An editable field also worth focusing.
        assertEquals(
            listOf("set_value", "focus"),
            PureRules.capabilities(
                clickable = false, hasClickAction = false, editable = true,
                hasSetTextAction = true, focusable = true, hasFocusAction = false,
                scrollable = false, hasScrollActions = false,
            ),
        )
        // A scrollable list.
        assertEquals(
            listOf("focus", "scroll"),
            PureRules.capabilities(
                clickable = false, hasClickAction = false, editable = false,
                hasSetTextAction = false, focusable = true, hasFocusAction = false,
                scrollable = true, hasScrollActions = true,
            ),
        )
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

package dev.anythinguse.lau.helper

import android.view.accessibility.AccessibilityNodeInfo

/**
 * Pure rules of the helper, deliberately free of framework *state* so they can be
 * unit tested on the JVM (`gradle :app:testDebugUnitTest`). The service calls
 * these; nothing here touches a socket, a node or the accessibility service.
 */
object PureRules {
    /** `adb forward` reaches the device as the shell uid. */
    const val SHELL_UID = 2000
    const val ROOT_UID = 0

    /**
     * Plan §4: only root/shell may talk to the helper socket. `null` means the
     * platform refused to report credentials — that must not break the working
     * path, so it is allowed (defence in depth, not a hard boundary).
     */
    fun peerUidAllowed(uid: Int?): Boolean =
        uid == null || uid == ROOT_UID || uid == SHELL_UID

    /** `android.widget.Button` → `Button`; blank or null → `View`. */
    fun shortRole(className: String?): String {
        if (className.isNullOrBlank()) return "View"
        val i = className.lastIndexOf('.')
        return if (i >= 0) className.substring(i + 1) else className
    }

    /**
     * Plan §5.4/§5.6: a credential field is **flagged, never read**. This is the
     * only path by which an editable node's text may enter an observation — the
     * dump and the `set_value` read-back both go through it, and a password node
     * yields nothing (not even to the operator).
     */
    fun observationValue(isEditable: Boolean, isPassword: Boolean, text: String?): String? =
        if (isEditable && !isPassword) text?.take(200) else null

    /**
     * What a node contributes to a derived label. A credential field contributes
     * its `contentDescription` (usually the hint) but **never its text**, so a
     * password cannot leak into an ancestor's label.
     */
    fun labelContribution(
        text: String?,
        contentDescription: String?,
        isPassword: Boolean,
    ): List<String> {
        val out = ArrayList<String>(2)
        if (!isPassword) {
            text?.trim()?.takeIf { it.isNotEmpty() }?.let { out.add(it.take(200)) }
        }
        contentDescription?.trim()?.takeIf { it.isNotEmpty() }?.let { out.add(it.take(200)) }
        return out
    }

    /**
     * Plan §2: a scroll request carries only an axis and a sign, and the helper
     * performs a single page step. Returns the accessibility action id, or null
     * when the delta is zero (the caller reports `unsupported_capability`).
     */
    fun scrollAction(dx: Double, dy: Double): Int? {
        val vertical = kotlin.math.abs(dy) >= kotlin.math.abs(dx)
        return when {
            vertical && dy > 0 -> AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
            vertical && dy < 0 -> AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
            !vertical && dx > 0 -> AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
            !vertical && dx < 0 -> AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
            else -> null
        }
    }

    /**
     * Plan §3: capability advertising. Only claim what the node actually offers,
     * because a claimed capability that fails at perform time is a dead end for
     * the actor (coordinates are refused, so there is no fallback).
     */
    fun capabilities(
        clickable: Boolean,
        hasClickAction: Boolean,
        editable: Boolean,
        hasSetTextAction: Boolean,
        focusable: Boolean,
        hasFocusAction: Boolean,
        scrollable: Boolean,
        hasScrollActions: Boolean,
    ): List<String> {
        val caps = ArrayList<String>(4)
        if (clickable || hasClickAction) caps.add("invoke")
        if (editable || hasSetTextAction) caps.add("set_value")
        if (focusable || hasFocusAction) caps.add("focus")
        if (scrollable || hasScrollActions) caps.add("scroll")
        return caps
    }
}

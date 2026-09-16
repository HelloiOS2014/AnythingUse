package dev.anythinguse.lau.helper

import android.accessibilityservice.AccessibilityService
import android.app.KeyguardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.os.Bundle
import android.os.PowerManager
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.net.LocalServerSocket
import android.net.LocalSocket
import org.json.JSONArray
import org.json.JSONObject
import java.io.BufferedReader
import java.io.BufferedWriter
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * AnythingUse LAU helper. Hosts a localabstract socket and executes semantic
 * accessibility actions. Coordinate input is dispatchGesture only (not ADB).
 */
class LauAccessibilityService : AccessibilityService() {
    private val running = AtomicBoolean(false)
    private var server: LocalServerSocket? = null
    private var acceptThread: Thread? = null

    /**
     * Random identity of *this service instance*. Every observation id carries
     * it, so an instance rebuild (screen off/on, rebind, crash) invalidates all
     * earlier observations even when the generation counter starts over.
     */
    @Volatile private var sessionId: String = ""

    @Volatile private var generation: Long = 0
    private val nodes = ArrayList<AccessibilityNodeInfo>(64)

    /** What the current dump advertised per element id (plan §5.2 steps 4–6). */
    private val meta = HashMap<String, ElementMeta>()
    private val lock = Any()

    private class ElementMeta(
        val packageName: String,
        val windowId: Int,
        val x: Double,
        val y: Double,
        val width: Double,
        val height: Double,
        val capabilities: Set<String>,
    )

    override fun onServiceConnected() {
        super.onServiceConnected()
        sessionId = newSessionId()
        startServer()
    }

    private fun newSessionId(): String =
        java.util.UUID.randomUUID().toString().replace("-", "").take(8)

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        // Window changes invalidate the dump generation; next dump is fresh.
    }

    override fun onInterrupt() {}

    override fun onDestroy() {
        stopServer()
        super.onDestroy()
    }

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        stopServer()
        return super.onUnbind(intent)
    }

    private fun startServer() {
        if (!running.compareAndSet(false, true)) return
        acceptThread = thread(name = "lau-helper-accept", isDaemon = true) {
            try {
                server = LocalServerSocket(SOCKET_NAME)
                while (running.get()) {
                    val client = try {
                        server?.accept() ?: break
                    } catch (_: Exception) {
                        break
                    }
                    try {
                        handleClient(client)
                    } catch (_: Exception) {
                    } finally {
                        try { client.close() } catch (_: Exception) {}
                    }
                }
            } catch (_: Exception) {
            } finally {
                running.set(false)
            }
        }
    }

    private fun stopServer() {
        running.set(false)
        try { server?.close() } catch (_: Exception) {}
        server = null
        acceptThread = null
        synchronized(lock) { recycleAll() }
    }

    private fun handleClient(socket: LocalSocket) {
        socket.soTimeout = 8_000
        val reader = BufferedReader(InputStreamReader(socket.inputStream, StandardCharsets.UTF_8))
        val writer = BufferedWriter(OutputStreamWriter(socket.outputStream, StandardCharsets.UTF_8))
        // Plan §4: the forwarded socket is reachable by other device-local
        // processes. Accept only root/shell (adb forward arrives as shell); if the
        // platform refuses to report credentials we do not break the working path.
        val peerUid = try {
            socket.peerCredentials?.uid
        } catch (_: Exception) {
            null
        }
        if (!PureRules.peerUidAllowed(peerUid)) {
            write(
                writer,
                error(JSONObject(), "forbidden_peer", "peer uid $peerUid is not allowed")
            )
            return
        }
        val line = reader.readLine() ?: return
        if (line.length > MAX_REQUEST) {
            write(writer, error(JSONObject(), "protocol_error", "request too large"))
            return
        }
        val req = try {
            JSONObject(line)
        } catch (_: Exception) {
            write(writer, error(JSONObject(), "protocol_error", "malformed json"))
            return
        }
        val id = req.optString("id", "")
        val v = req.optInt("v", 1)
        if (v != 1) {
            write(writer, error(req, "protocol_error", "unsupported protocol version $v"))
            return
        }
        val op = req.optString("op", "")
        val resp = try {
            when (op) {
                "ping" -> ok(
                    req,
                    JSONObject()
                        .put("pong", true)
                        .put("generation", generation)
                        .put("sessionId", sessionId)
                )
                "dump" -> dump(req)
                "foreground" -> foreground(req)
                "app_identity" -> appIdentity(req)
                "invoke" -> invoke(req)
                "set_value" -> setValue(req)
                "scroll" -> scroll(req)
                "global_back" -> globalBack(req)
                "launch" -> launch(req)
                else -> error(req, "protocol_error", "unknown op $op")
            }
        } catch (e: HelperException) {
            error(req, e.code, e.message ?: e.code)
        } catch (e: Exception) {
            error(req, "internal", e.message ?: "internal error")
        }
        if (id.isNotEmpty()) resp.put("id", id)
        write(writer, resp)
    }

    private fun write(writer: BufferedWriter, obj: JSONObject) {
        writer.write(obj.toString())
        writer.write("\n")
        writer.flush()
    }

    private fun ok(req: JSONObject, data: JSONObject): JSONObject {
        val out = JSONObject()
        out.put("v", 1)
        out.put("id", req.optString("id", ""))
        out.put("ok", true)
        out.put("data", data)
        return out
    }

    private fun error(req: JSONObject, code: String, message: String): JSONObject {
        val out = JSONObject()
        out.put("v", 1)
        out.put("id", req.optString("id", ""))
        out.put("ok", false)
        out.put("error", JSONObject().put("code", code).put("message", message))
        return out
    }

    private fun screenState(): JSONObject {
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        val km = getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager
        val dm = resources.displayMetrics
        val display = try {
            (getSystemService(Context.DISPLAY_SERVICE) as DisplayManager)
                .getDisplay(Display.DEFAULT_DISPLAY)
        } catch (_: Exception) {
            null
        }
        return JSONObject()
            .put("isInteractive", pm.isInteractive)
            .put("keyguardLocked", km.isKeyguardLocked)
            .put("screenWidth", dm.widthPixels)
            .put("screenHeight", dm.heightPixels)
            .put("rotation", display?.rotation ?: -1)
            .put("displayId", display?.displayId ?: -1)
    }

    private fun dump(req: JSONObject): JSONObject {
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        val km = getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager
        if (!pm.isInteractive) throw HelperException("screen_off", "display is not interactive")
        if (km.isKeyguardLocked) throw HelperException("device_locked", "device is locked")

        val root = rootInActiveWindow
            ?: throw HelperException("target_lost", "no active accessibility window")
        val pkg = root.packageName?.toString() ?: ""
        if (pkg == packageName) {
            root.recycle()
            throw HelperException("forbidden_package", "refusing to automate the helper itself")
        }

        // Plan §5.3 / finding #22: the observation must carry real window identity.
        // `AccessibilityWindowInfo.title` is the usable title; the root node's
        // contentDescription is empty on this platform.
        val rootWindowId = root.windowId
        val windowTitle = try {
            windows.firstOrNull { it.id == rootWindowId }?.title?.toString() ?: ""
        } catch (_: Exception) {
            ""
        }

        val dm = resources.displayMetrics
        val sw = dm.widthPixels.coerceAtLeast(1).toDouble()
        val sh = dm.heightPixels.coerceAtLeast(1).toDouble()
        val collected = JSONArray()
        synchronized(lock) {
            recycleAll()
            generation += 1
            walk(root, collected, sw, sh)
        }
        val data = screenState()
            .put("observationId", "$sessionId:$generation")
            .put("packageName", pkg)
            .put("windowId", rootWindowId)
            .put("windowTitle", windowTitle)
            .put("capturedAtMs", System.currentTimeMillis())
            .put("elements", collected)
        return ok(req, data)
    }

    private fun walk(node: AccessibilityNodeInfo, out: JSONArray, sw: Double, sh: Double) {
        if (out.length() >= MAX_NODES) return
        if (interesting(node)) {
            val bounds = Rect()
            node.getBoundsInScreen(bounds)
            if (bounds.width() > 0 && bounds.height() > 0) {
                val id = "e${nodes.size + 1}"
                nodes.add(AccessibilityNodeInfo.obtain(node))
                val caps = PureRules.capabilities(
                    clickable = node.isClickable,
                    hasClickAction = hasAction(node, AccessibilityNodeInfo.ACTION_CLICK),
                    editable = node.isEditable,
                    hasSetTextAction = hasAction(node, AccessibilityNodeInfo.ACTION_SET_TEXT),
                    focusable = node.isFocusable,
                    hasFocusAction = hasAction(node, AccessibilityNodeInfo.ACTION_FOCUS),
                    scrollable = node.isScrollable,
                    hasScrollActions = hasAction(node, AccessibilityNodeInfo.ACTION_SCROLL_FORWARD) ||
                        hasAction(node, AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD),
                ).toCollection(linkedSetOf())
                val nx = bounds.left / sw
                val ny = bounds.top / sh
                val nw = bounds.width() / sw
                val nh = bounds.height() / sh
                meta[id] = ElementMeta(
                    node.packageName?.toString() ?: "",
                    node.windowId,
                    nx,
                    ny,
                    nw,
                    nh,
                    caps.toSet(),
                )
                val capsJson = JSONArray()
                for (cap in caps) capsJson.put(cap)
                // A credential field never names itself with its own text.
                val ownLabel = PureRules.labelContribution(
                    node.text?.toString(),
                    node.contentDescription?.toString(),
                    node.isPassword,
                ).firstOrNull()
                // Plan §3 label attribution: a clickable row often has no text of
                // its own; derive it from the subtree so the target is named.
                val label = if (ownLabel.isNullOrBlank()) derivedLabel(node) else ownLabel
                val obj = JSONObject()
                    .put("id", id)
                    .put("role", shortRole(node.className?.toString()))
                    .put("frame", JSONObject()
                        .put("x", nx)
                        .put("y", ny)
                        .put("width", nw)
                        .put("height", nh))
                    .put("capabilities", capsJson)
                if (!label.isNullOrBlank()) obj.put("label", label.take(200))
                // A credential field is flagged, never read (§5.4 / privacy).
                PureRules.observationValue(
                    node.isEditable,
                    node.isPassword,
                    node.text?.toString(),
                )?.let { obj.put("value", it) }
                if (node.isPassword) obj.put("password", true)
                out.put(obj)
            }
        }
        for (i in 0 until node.childCount) {
            val child = node.getChild(i) ?: continue
            walk(child, out, sw, sh)
            child.recycle()
        }
    }

    private fun interesting(node: AccessibilityNodeInfo): Boolean {
        if (node.isClickable || node.isEditable || node.isScrollable || node.isCheckable) return true
        if (hasAction(node, AccessibilityNodeInfo.ACTION_CLICK)) return true
        if (hasAction(node, AccessibilityNodeInfo.ACTION_SET_TEXT)) return true
        val t = node.text?.toString()
        val d = node.contentDescription?.toString()
        return !t.isNullOrBlank() || !d.isNullOrBlank()
    }

    private fun hasAction(node: AccessibilityNodeInfo, action: Int): Boolean {
        return node.actionList.any { it.id == action }
    }

    /**
     * Plan §3 label attribution (2026-09-15): collect non-empty text from the
     * subtree (depth ≤ LABEL_DEPTH) so a clickable row with no text of its own
     * still carries a name. Bounded to LABEL_PARTS fragments / 200 chars.
     */
    private fun derivedLabel(node: AccessibilityNodeInfo): String {
        val parts = ArrayList<String>(LABEL_PARTS)
        fun collect(n: AccessibilityNodeInfo, depth: Int) {
            if (parts.size >= LABEL_PARTS || depth > LABEL_DEPTH) return
            // A credential field contributes no text to any label (not even its
            // ancestors'): only its contentDescription (usually the hint).
            for (v in PureRules.labelContribution(
                n.text?.toString(),
                n.contentDescription?.toString(),
                n.isPassword,
            )) {
                val s = v.trim()
                if (s.isNotEmpty() && parts.none { it == s }) parts.add(s)
                if (parts.size >= LABEL_PARTS) return
            }
            for (i in 0 until n.childCount) {
                val child = n.getChild(i) ?: continue
                collect(child, depth + 1)
                child.recycle()
                if (parts.size >= LABEL_PARTS) return
            }
        }
        collect(node, 0)
        return parts.joinToString(" ").take(200)
    }

    private fun shortRole(className: String?): String = PureRules.shortRole(className)

    private fun invoke(req: JSONObject): JSONObject {
        val node = resolveNode(req, "invoke")
        if (!node.isClickable && !hasAction(node, AccessibilityNodeInfo.ACTION_CLICK)) {
            throw HelperException("unsupported_capability", "${req.optString("elementId")} has no invoke")
        }
        val ok = node.performAction(AccessibilityNodeInfo.ACTION_CLICK)
        if (!ok) throw HelperException("verification_failed", "ACTION_CLICK returned false")
        return ok(req, JSONObject().put("performed", "invoke"))
    }

    private fun setValue(req: JSONObject): JSONObject {
        val node = resolveNode(req, "set_value")
        if (!node.isEditable && !hasAction(node, AccessibilityNodeInfo.ACTION_SET_TEXT)) {
            throw HelperException("unsupported_capability", "${req.optString("elementId")} has no set_value")
        }
        val text = req.optString("text", "")
        val args = Bundle()
        args.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text)
        val ok = node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
        if (!ok) throw HelperException("verification_failed", "ACTION_SET_TEXT returned false")
        node.refresh()
        val got = node.text?.toString() ?: ""
        if (text.isNotEmpty() && got != text) {
            throw HelperException(
                "verification_failed",
                if (node.isPassword) "set_value did not stick (credential field)"
                else "set_value did not stick (got ${got.take(40)})"
            )
        }
        // Never echo a credential back, not even to the operator.
        if (node.isPassword) {
            return ok(req, JSONObject().put("performed", "set_value").put("value", "<redacted>"))
        }
        return ok(req, JSONObject().put("performed", "set_value").put("value", got))
    }

    private fun scroll(req: JSONObject): JSONObject {
        val node = resolveNode(req, "scroll")
        val dx = req.optDouble("dx", 0.0)
        val dy = req.optDouble("dy", 0.0)
        val action = PureRules.scrollAction(dx, dy)
            ?: throw HelperException("unsupported_capability", "scroll delta is zero")
        if (!node.isScrollable && !hasAction(node, action)) {
            throw HelperException("unsupported_capability", "${req.optString("elementId")} has no scroll")
        }
        val ok = node.performAction(action)
        if (!ok) throw HelperException("verification_failed", "scroll returned false")
        return ok(req, JSONObject().put("performed", "scroll"))
    }

    /**
     * Plan §3: system-level back, for when the app exposes no semantic back
     * control. Honest failure if the platform refuses it.
     */
    private fun globalBack(req: JSONObject): JSONObject {
        val performed = performGlobalAction(GLOBAL_ACTION_BACK)
        if (!performed) {
            throw HelperException("verification_failed", "GLOBAL_ACTION_BACK returned false")
        }
        return ok(req, JSONObject().put("performed", "global_back"))
    }

    /**
     * Plan §5.4: stable identity of an installed package — package name plus the
     * SHA-256 of its signing certificate. Survives app updates; changing the
     * signer changes the identity (and therefore invalidates a persisted grant).
     */
    private fun appIdentity(req: JSONObject): JSONObject {
        val pkg = req.optString("packageName", "")
        if (pkg.isEmpty()) throw HelperException("protocol_error", "packageName required")
        val info = try {
            packageManager.getPackageInfo(pkg, PackageManager.GET_SIGNING_CERTIFICATES)
        } catch (_: Exception) {
            throw HelperException("target_lost", "package $pkg is not installed")
        }
        val certSha256 = try {
            val signer = info.signingInfo?.apkContentsSigners?.firstOrNull()
            val bytes = signer?.toByteArray()
            if (bytes == null) {
                ""
            } else {
                MessageDigest.getInstance("SHA-256").digest(bytes)
                    .joinToString("") { "%02x".format(it) }
            }
        } catch (_: Exception) {
            ""
        }
        val label = try {
            packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, 0)).toString()
        } catch (_: Exception) {
            pkg
        }
        return ok(
            req,
            JSONObject()
                .put("packageName", pkg)
                .put("label", label)
                .put("certSha256", certSha256)
        )
    }

    private fun foreground(req: JSONObject): JSONObject {
        val root = rootInActiveWindow
        val pkg = root?.packageName?.toString() ?: ""
        val title = root?.contentDescription?.toString() ?: ""
        root?.recycle()
        val data = screenState()
            .put("packageName", pkg)
            .put("windowTitle", title)
        return ok(req, data)
    }

    private fun launch(req: JSONObject): JSONObject {
        val pkg = req.optString("packageName", "")
        if (pkg.isEmpty()) {
            throw HelperException("protocol_error", "packageName required")
        }
        if (pkg == packageName) {
            throw HelperException("forbidden_package", "refusing to automate the helper itself")
        }
        val intent = packageManager.getLaunchIntentForPackage(pkg)
            ?: throw HelperException("target_lost", "no launch intent for $pkg")
        intent.addFlags(
            Intent.FLAG_ACTIVITY_NEW_TASK or
                Intent.FLAG_ACTIVITY_CLEAR_TOP or
                Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED or
                Intent.FLAG_ACTIVITY_REORDER_TO_FRONT
        )
        startActivity(intent)
        return ok(req, JSONObject().put("launched", pkg))
    }

    private fun recycleAll() {
        for (n in nodes) {
            try { n.recycle() } catch (_: Exception) {}
        }
        nodes.clear()
        meta.clear()
    }

    private fun screenSize(): Pair<Double, Double> {
        val dm = resources.displayMetrics
        return Pair(
            dm.widthPixels.coerceAtLeast(1).toDouble(),
            dm.heightPixels.coerceAtLeast(1).toDouble(),
        )
    }

    /**
     * Plan §5.2 (2026-09-15 修订) — resolve the node an observation-bound action
     * refers to, or refuse. Never silently substitutes another node.
     */
    private fun resolveNode(req: JSONObject, requiredCapability: String): AccessibilityNodeInfo {
        val token = req.optString("observationId", "")
        val eid = req.optString("elementId", "")
        if (token.isEmpty() || eid.isEmpty()) {
            throw HelperException("protocol_error", "observationId and elementId required")
        }
        val sep = token.indexOf(':')
        if (sep <= 0) {
            throw HelperException("protocol_error", "malformed observationId (want <session>:<generation>)")
        }
        val sess = token.substring(0, sep)
        val gen = token.substring(sep + 1).toLongOrNull()
            ?: throw HelperException("protocol_error", "malformed observationId generation")
        synchronized(lock) {
            // 1–2. instance session, then generation: either mismatch is stale.
            if (sessionId.isEmpty() || sess != sessionId) {
                throw HelperException(
                    "stale_observation",
                    "observation belongs to session $sess, current is $sessionId"
                )
            }
            if (gen != generation) {
                throw HelperException(
                    "stale_observation",
                    "observation $token is not current ($sessionId:$generation)"
                )
            }
            // 3. index within this dump, and the node must still refresh.
            val idx = eid.removePrefix("e").toIntOrNull()?.minus(1)
                ?: throw HelperException("element_not_found", "bad element id $eid")
            if (idx < 0 || idx >= nodes.size) {
                throw HelperException("element_not_found", "element $eid not in dump")
            }
            val recorded = meta[eid]
                ?: throw HelperException("stale_observation", "no snapshot recorded for $eid")
            val node = nodes[idx]
            if (!node.refresh()) {
                throw HelperException("stale_observation", "node $eid failed refresh")
            }
            // 4. identity: same package and same window as the observation.
            val pkg = node.packageName?.toString() ?: ""
            if (pkg == packageName) {
                throw HelperException("forbidden_package", "refusing to automate the helper itself")
            }
            if (pkg != recorded.packageName) {
                throw HelperException("stale_observation", "node $eid now belongs to $pkg")
            }
            if (node.windowId != recorded.windowId) {
                throw HelperException(
                    "stale_observation",
                    "node $eid moved to window ${node.windowId} (was ${recorded.windowId})"
                )
            }
            // 5. bounds: normalized frame within tolerance (D7 option ②).
            val b = Rect()
            node.getBoundsInScreen(b)
            val (sw, sh) = screenSize()
            val moved = kotlin.math.abs(b.left / sw - recorded.x) > BOUNDS_TOLERANCE ||
                kotlin.math.abs(b.top / sh - recorded.y) > BOUNDS_TOLERANCE ||
                kotlin.math.abs(b.width() / sw - recorded.width) > BOUNDS_TOLERANCE ||
                kotlin.math.abs(b.height() / sh - recorded.height) > BOUNDS_TOLERANCE
            if (moved) {
                throw HelperException("stale_observation", "node $eid moved or resized since the dump")
            }
            // 6. capability: only what the dump advertised.
            if (requiredCapability !in recorded.capabilities) {
                throw HelperException(
                    "unsupported_capability",
                    "$eid did not advertise $requiredCapability"
                )
            }
            return node
        }
    }

    private class HelperException(val code: String, message: String) : Exception(message)

    companion object {
        const val SOCKET_NAME = "dev.anythinguse.lau.helper"
        const val MAX_NODES = 400
        const val MAX_REQUEST = 64 * 1024

        /** Normalized frame tolerance for the §5.2 bounds re-check (D7 option ②). */
        const val BOUNDS_TOLERANCE = 0.005

        /** Label attribution bounds (plan §3): subtree depth and fragment count. */
        const val LABEL_DEPTH = 3
        const val LABEL_PARTS = 3
    }
}

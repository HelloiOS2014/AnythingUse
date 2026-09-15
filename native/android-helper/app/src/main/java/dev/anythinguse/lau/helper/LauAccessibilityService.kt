package dev.anythinguse.lau.helper

import android.accessibilityservice.AccessibilityService
import android.app.KeyguardManager
import android.content.Context
import android.content.Intent
import android.graphics.Rect
import android.os.Bundle
import android.os.PowerManager
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

    @Volatile private var generation: Long = 0
    private val nodes = ArrayList<AccessibilityNodeInfo>(64)
    private val lock = Any()

    override fun onServiceConnected() {
        super.onServiceConnected()
        startServer()
    }

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
                "ping" -> ok(req, JSONObject().put("pong", true).put("generation", generation))
                "dump" -> dump(req)
                "foreground" -> foreground(req)
                "invoke" -> invoke(req)
                "set_value" -> setValue(req)
                "scroll" -> scroll(req)
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
        return JSONObject()
            .put("isInteractive", pm.isInteractive)
            .put("keyguardLocked", km.isKeyguardLocked)
            .put("screenWidth", dm.widthPixels)
            .put("screenHeight", dm.heightPixels)
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
            .put("observationId", generation)
            .put("packageName", pkg)
            .put("windowTitle", root.contentDescription?.toString() ?: "")
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
                val caps = JSONArray()
                if (node.isClickable || hasAction(node, AccessibilityNodeInfo.ACTION_CLICK)) {
                    caps.put("invoke")
                }
                if (node.isEditable || hasAction(node, AccessibilityNodeInfo.ACTION_SET_TEXT)) {
                    caps.put("set_value")
                }
                if (hasAction(node, AccessibilityNodeInfo.ACTION_FOCUS) || node.isFocusable) {
                    caps.put("focus")
                }
                if (node.isScrollable ||
                    hasAction(node, AccessibilityNodeInfo.ACTION_SCROLL_FORWARD) ||
                    hasAction(node, AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD)
                ) {
                    caps.put("scroll")
                }
                val label = node.text?.toString() ?: node.contentDescription?.toString()
                val obj = JSONObject()
                    .put("id", id)
                    .put("role", shortRole(node.className?.toString()))
                    .put("frame", JSONObject()
                        .put("x", bounds.left / sw)
                        .put("y", bounds.top / sh)
                        .put("width", bounds.width() / sw)
                        .put("height", bounds.height() / sh))
                    .put("capabilities", caps)
                if (!label.isNullOrBlank()) obj.put("label", label.take(200))
                if (node.isEditable && node.text != null) obj.put("value", node.text.toString().take(200))
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

    private fun shortRole(className: String?): String {
        if (className.isNullOrBlank()) return "View"
        val i = className.lastIndexOf('.')
        return if (i >= 0) className.substring(i + 1) else className
    }

    private fun requireNode(req: JSONObject): AccessibilityNodeInfo {
        val obs = req.optLong("observationId", -1)
        val eid = req.optString("elementId", "")
        if (obs <= 0 || eid.isEmpty()) {
            throw HelperException("protocol_error", "observationId and elementId required")
        }
        synchronized(lock) {
            if (obs != generation) {
                throw HelperException("stale_observation", "observation $obs is not current ($generation)")
            }
            val idx = eid.removePrefix("e").toIntOrNull()?.minus(1)
                ?: throw HelperException("element_not_found", "bad element id $eid")
            if (idx < 0 || idx >= nodes.size) {
                throw HelperException("element_not_found", "element $eid not in dump")
            }
            val node = nodes[idx]
            if (!node.refresh()) {
                throw HelperException("stale_observation", "node $eid failed refresh")
            }
            val pkg = node.packageName?.toString() ?: ""
            if (pkg == packageName) {
                throw HelperException("forbidden_package", "refusing to automate the helper itself")
            }
            return node
        }
    }

    private fun invoke(req: JSONObject): JSONObject {
        val node = requireNode(req)
        if (!node.isClickable && !hasAction(node, AccessibilityNodeInfo.ACTION_CLICK)) {
            throw HelperException("unsupported_capability", "${req.optString("elementId")} has no invoke")
        }
        val ok = node.performAction(AccessibilityNodeInfo.ACTION_CLICK)
        if (!ok) throw HelperException("verification_failed", "ACTION_CLICK returned false")
        return ok(req, JSONObject().put("performed", "invoke"))
    }

    private fun setValue(req: JSONObject): JSONObject {
        val node = requireNode(req)
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
                "set_value did not stick (got ${got.take(40)})"
            )
        }
        return ok(req, JSONObject().put("performed", "set_value").put("value", got))
    }

    private fun scroll(req: JSONObject): JSONObject {
        val node = requireNode(req)
        val dx = req.optDouble("dx", 0.0)
        val dy = req.optDouble("dy", 0.0)
        val action = when {
            kotlin.math.abs(dy) >= kotlin.math.abs(dx) && dy > 0 ->
                AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
            kotlin.math.abs(dy) >= kotlin.math.abs(dx) && dy < 0 ->
                AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
            dx > 0 -> AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
            dx < 0 -> AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
            else -> throw HelperException("unsupported_capability", "scroll delta is zero")
        }
        if (!node.isScrollable && !hasAction(node, action)) {
            throw HelperException("unsupported_capability", "${req.optString("elementId")} has no scroll")
        }
        val ok = node.performAction(action)
        if (!ok) throw HelperException("verification_failed", "scroll returned false")
        return ok(req, JSONObject().put("performed", "scroll"))
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
    }

    private class HelperException(val code: String, message: String) : Exception(message)

    companion object {
        const val SOCKET_NAME = "dev.anythinguse.lau.helper"
        const val MAX_NODES = 400
        const val MAX_REQUEST = 64 * 1024
    }
}

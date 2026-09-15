package dev.anythinguse.lau.helper

import android.accessibilityservice.AccessibilityServiceInfo
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.provider.Settings
import android.view.accessibility.AccessibilityManager
import android.widget.Button
import android.widget.TextView
import android.app.Activity

class LauActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        findViewById<Button>(R.id.open_settings).setOnClickListener {
            startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
        }
        refreshStatus()
    }

    override fun onResume() {
        super.onResume()
        refreshStatus()
    }

    private fun refreshStatus() {
        val on = isServiceEnabled(this)
        findViewById<TextView>(R.id.status).setText(
            if (on) R.string.status_on else R.string.status_off
        )
    }

    companion object {
        fun isServiceEnabled(context: Context): Boolean {
            val am = context.getSystemService(Context.ACCESSIBILITY_SERVICE) as AccessibilityManager
            val list = am.getEnabledAccessibilityServiceList(AccessibilityServiceInfo.FEEDBACK_GENERIC)
            val want = "${context.packageName}/.LauAccessibilityService"
            val wantFqcn = "${context.packageName}/${LauAccessibilityService::class.java.name}"
            return list.any {
                val id = it.id
                id == want || id == wantFqcn || id.contains("LauAccessibilityService")
            }
        }
    }
}

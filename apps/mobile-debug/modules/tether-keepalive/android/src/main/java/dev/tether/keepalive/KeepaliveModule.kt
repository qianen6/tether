package dev.tether.keepalive

import android.content.Intent
import androidx.core.content.ContextCompat
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition

class KeepaliveModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("TetherKeepalive")

    Function("start") { title: String, text: String ->
      val context = appContext.reactContext ?: return@Function false
      val intent = Intent(context, KeepaliveService::class.java)
        .putExtra(KeepaliveService.EXTRA_TITLE, title)
        .putExtra(KeepaliveService.EXTRA_TEXT, text)
      ContextCompat.startForegroundService(context, intent)
      true
    }

    Function("stop") {
      val context = appContext.reactContext ?: return@Function false
      context.stopService(Intent(context, KeepaliveService::class.java))
    }
  }
}

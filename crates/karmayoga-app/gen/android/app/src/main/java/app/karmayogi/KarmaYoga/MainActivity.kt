package app.karmayogi.KarmaYoga

import android.os.Bundle
import android.view.View
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

/**
 * The phone shell's one window: Tauri's activity, drawn edge to edge, with the
 * screen's edges told to the page.
 *
 * Edge to edge, the page runs under the status bar and the gesture bar, and
 * has to keep its own controls clear of them. CSS's `env(safe-area-inset-*)`
 * is meant to say how far, but Android's webview says it for some edges and
 * versions and not others (133 gives the status bar and not the gesture
 * bar). So this measures them itself and hands them over, in CSS pixels:
 * `window.KarmaYogaInsets.get()` when the page starts, and a
 * `karmayoga:insets` event whenever they change -- the phone turned, the
 * keyboard up. `ui/src/lib/phone.svelte.ts` turns them into `--safe-*`.
 *
 * The keyboard pushes the page up rather than covering it: the window's
 * content is padded by the keyboard's height, so the webview is that much
 * shorter and the field being typed in stays on screen.
 */
class MainActivity : TauriActivity() {
  @Volatile private var edges = """{"top":0,"right":0,"bottom":0,"left":0,"keyboard":0}"""

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }

  override fun onWebViewCreate(webView: WebView) {
    webView.addJavascriptInterface(Edges(), "KarmaYogaInsets")
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
      )
      val keyboard = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
      view.setPadding(0, 0, 0, keyboard)
      val d = resources.displayMetrics.density
      // Over the keyboard the gesture bar is the keyboard's business: the
      // page ends at its top edge.
      val bottom = if (keyboard > 0) 0 else bars.bottom
      edges = """{"top":${bars.top / d},"right":${bars.right / d},"bottom":${bottom / d},""" +
        """"left":${bars.left / d},"keyboard":${keyboard / d}}"""
      val event = "window.dispatchEvent(new CustomEvent('karmayoga:insets', { detail: $edges }))"
      webView.post { webView.evaluateJavascript(event, null) }
      // The keyboard is handled here, so the webview is not told of it as
      // well and does not make room for it a second time.
      WindowInsetsCompat.Builder(insets)
        .setInsets(WindowInsetsCompat.Type.ime(), Insets.NONE)
        .build()
    }
  }

  /** What the page reads at startup, before any change has been sent. */
  inner class Edges {
    @JavascriptInterface
    fun get(): String = edges
  }
}

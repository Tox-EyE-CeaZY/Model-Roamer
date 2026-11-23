package app.modelroamer

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.EditorInfo
import android.webkit.CookieManager
import android.webkit.JavascriptInterface
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONObject
import java.net.DatagramPacket
import java.net.DatagramSocket
import kotlin.concurrent.thread

/**
 * Model Roamer for Android: a window onto a Model Roamer running on a computer (or server) you control. The app itself
 * is the same interface as in the desktop app and the browser; this wraps it, remembers the address, finds the
 * computer on your network, and opens links and file pickers the way a phone app should.
 */
class MainActivity : Activity() {
    private val bg = Color.parseColor("#0a1113")
    private val ink = Color.parseColor("#e7efe9")
    private val dim = Color.parseColor("#8fa39a")
    private val accent = Color.parseColor("#c8ff3d")

    private var web: WebView? = null
    private var server: String = ""
    private var picker: ValueCallback<Array<Uri>>? = null

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        server = prefs().getString("server", "") ?: ""
        if (server.isEmpty()) showSetup() else showWeb()
    }

    private fun prefs() = getSharedPreferences("roamer", Context.MODE_PRIVATE)

    // ---------------------------------------------------------------- the interface

    @SuppressLint("SetJavaScriptEnabled")
    private fun showWeb() {
        val w = WebView(this)
        web = w
        w.setBackgroundColor(bg)
        w.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            allowFileAccess = false
            allowContentAccess = false
            mediaPlaybackRequiresUserGesture = true
            mixedContentMode = WebSettings.MIXED_CONTENT_ALWAYS_ALLOW
            userAgentString = userAgentString + " ModelRoamerAndroid/1"
        }
        CookieManager.getInstance().setAcceptCookie(true)
        CookieManager.getInstance().setAcceptThirdPartyCookies(w, false)
        w.addJavascriptInterface(Bridge(), "RoamerAndroid")
        w.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(v: WebView, r: WebResourceRequest): Boolean {
                val u = r.url
                if (sameServer(u)) return false
                openOutside(u.toString())
                return true
            }

            override fun onReceivedError(v: WebView, r: WebResourceRequest, e: WebResourceError) {
                if (r.isForMainFrame) showError(e.description?.toString() ?: "Couldn't reach it.")
            }

            override fun onPageFinished(v: WebView, url: String) {
                CookieManager.getInstance().flush()
            }
        }
        w.webChromeClient = object : WebChromeClient() {
            override fun onShowFileChooser(v: WebView, cb: ValueCallback<Array<Uri>>, p: FileChooserParams): Boolean {
                picker?.onReceiveValue(null)
                picker = cb
                return try {
                    startActivityForResult(p.createIntent(), PICK)
                    true
                } catch (e: Exception) {
                    picker = null
                    false
                }
            }
        }
        setContentView(w)
        w.loadUrl(server)
    }

    private fun sameServer(u: Uri): Boolean {
        val s = Uri.parse(server)
        return u.host == s.host && (u.port == s.port || (u.port == -1 && s.port == -1))
    }

    private fun openOutside(url: String) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (e: Exception) { /* nothing can open it */ }
    }

    /** What the web interface can ask the app to do. */
    inner class Bridge {
        @JavascriptInterface fun changeServer() = runOnUiThread { showSetup() }
        @JavascriptInterface fun openExternal(url: String) = runOnUiThread { openOutside(url) }
        @JavascriptInterface fun server(): String = server
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(code: Int, result: Int, data: Intent?) {
        if (code == PICK) {
            picker?.onReceiveValue(if (result == RESULT_OK) android.webkit.WebChromeClient.FileChooserParams.parseResult(result, data) else null)
            picker = null
        } else super.onActivityResult(code, result, data)
    }

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        val w = web
        if (w != null && w.visibility == View.VISIBLE && w.canGoBack()) w.goBack() else super.onBackPressed()
    }

    override fun onPause() {
        super.onPause()
        CookieManager.getInstance().flush()
    }

    // ---------------------------------------------------------------- not connected

    private fun showError(why: String) {
        val box = column()
        box.addView(title("Can’t reach it"))
        box.addView(text("$server\n\n$why\n\nIs the computer awake, running Model Roamer with sharing on, and on the same network (or Tailscale)?"))
        box.addView(button("Try again") { showWeb() })
        box.addView(button("Change the address") { showSetup() })
        setContentView(ScrollView(this).apply { setBackgroundColor(bg); addView(box) })
    }

    private fun showSetup() {
        web?.destroy()
        web = null
        val box = column()
        box.addView(title("Model Roamer"))
        box.addView(text("Connect to the computer (or server) where Model Roamer runs. On it, open Settings, then Remote access, and turn sharing on."))
        val address = EditText(this).apply {
            hint = "http://192.168.1.20:4630"
            setText(server)
            setTextColor(ink)
            setHintTextColor(dim)
            setSingleLine()
            inputType = android.text.InputType.TYPE_TEXT_VARIATION_URI
            imeOptions = EditorInfo.IME_ACTION_GO
        }
        box.addView(address, lp(top = 24))
        val connect = { connectTo(address.text.toString()) }
        address.setOnEditorActionListener { _, _, _ -> connect(); true }
        box.addView(button("Connect") { connect() }, lp(top = 12))
        val found = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val look = button("Find it on my network") { }
        look.setOnClickListener {
            look.isEnabled = false
            look.text = "Looking…"
            found.removeAllViews()
            thread { 
                val list = discover()
                runOnUiThread {
                    look.isEnabled = true
                    look.text = "Find it on my network"
                    if (list.isEmpty()) found.addView(text("Nothing answered. It can take a few seconds; check that sharing is on and “discoverable” is allowed, or type the address."))
                    for (s in list) found.addView(button("${s.optString("name")}  ·  ${s.optString("url")}") { connectTo(s.optString("url")) })
                }
            }
        }
        box.addView(look, lp(top = 12))
        box.addView(found, lp(top = 8))
        box.addView(text("The first time, the app asks you to pair: the computer shows a code under Settings, Remote access."), lp(top = 24))
        setContentView(ScrollView(this).apply { setBackgroundColor(bg); addView(box) })
    }

    private fun connectTo(raw: String) {
        var u = raw.trim().trimEnd('/')
        if (u.isEmpty()) return
        if (!u.startsWith("http://") && !u.startsWith("https://")) u = "http://$u"
        server = u
        prefs().edit().putString("server", u).apply()
        showWeb()
    }

    /** Servers announce themselves on the local network (UDP port 4631) every few seconds. */
    private fun discover(): List<JSONObject> {
        val out = ArrayList<JSONObject>()
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
        val lock = wifi?.createMulticastLock("roamer")?.apply { setReferenceCounted(false); acquire() }
        try {
            val sock = DatagramSocket(null).apply { reuseAddress = true; bind(java.net.InetSocketAddress(4631)); soTimeout = 1000 }
            val end = System.currentTimeMillis() + 5000
            val buf = ByteArray(2048)
            while (System.currentTimeMillis() < end) {
                try {
                    val p = DatagramPacket(buf, buf.size)
                    sock.receive(p)
                    val j = JSONObject(String(p.data, 0, p.length))
                    if (j.optString("app") != "model-roamer") continue
                    val host = p.address.hostAddress ?: continue
                    val url = (if (j.optBoolean("tls")) "https" else "http") + "://" + (if (host.contains(":")) "[$host]" else host) + ":" + j.optInt("port")
                    if (out.none { it.optString("id") == j.optString("id") }) out.add(j.put("url", url))
                } catch (e: java.net.SocketTimeoutException) { /* keep listening */ } catch (e: Exception) { /* not ours */ }
            }
            sock.close()
        } catch (e: Exception) { /* can't listen */ } finally {
            lock?.release()
        }
        return out
    }

    // ---------------------------------------------------------------- small views

    private fun column() = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(24), dp(56), dp(24), dp(32))
        setBackgroundColor(bg)
    }

    private fun title(s: String) = TextView(this).apply {
        text = s
        setTextColor(ink)
        textSize = 28f
        typeface = Typeface.DEFAULT_BOLD
    }

    private fun text(s: String) = TextView(this).apply {
        text = s
        setTextColor(dim)
        textSize = 15f
        setPadding(0, dp(12), 0, 0)
    }

    private fun button(label: String, onClick: () -> Unit) = Button(this).apply {
        text = label
        isAllCaps = false
        setTextColor(Color.parseColor("#0a1113"))
        setBackgroundColor(accent)
        gravity = Gravity.CENTER
        setOnClickListener { onClick() }
    }

    private fun lp(top: Int = 0) = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(top) }

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    companion object {
        private const val PICK = 7001
    }
}
